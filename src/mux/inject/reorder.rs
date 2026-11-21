//! Video frames from decode order back to presentation order.

use super::Frame;
use crate::pts_delta;
use std::collections::VecDeque;

/// Puts video frames, which arrive in decode order, back in presentation
/// order, so a scheduler can make per-frame metadata with non-decreasing
/// PTS even for video with B-frames.
///
/// A frame with PTS `p` is released once a frame with DTS at least `p` has
/// been seen: every later frame has a DTS at least as large and a PTS at
/// least its DTS, so none of them can be presented before `p`. A frame
/// without a DTS has DTS = PTS (ISO/IEC 13818-1 2.4.3.7). Timestamps are
/// compared modulo 2^33 (DECISIONS D-ISO13818-17). At most `max_depth`
/// frames wait; beyond that the earliest is released anyway, so a stream
/// whose timestamps never satisfy the rule cannot grow it. A frame flagged
/// [`Frame::discontinuity`] starts a new time base: frames of the old one
/// still waiting are discarded (counted by [`discarded`](Self::discarded)),
/// because metadata stamped with their PTS could only reach the stream
/// after the discontinuity, which R-ISO13818-031 forbids.
///
/// ```
/// use mpegts::mux::{Frame, FrameReorder};
///
/// // I0 P3 B1 B2 in decode order (PTS 1, 4, 2, 3; DTS 0, 1, 2, 3).
/// let mut reorder = FrameReorder::new(16);
/// let mut shown = Vec::new();
/// for (pts, dts) in [(1, 0), (4, 1), (2, 2), (3, 3)] {
///     reorder.push(Frame::new(0x100, pts).with_dts(dts));
///     while let Some(f) = reorder.pop() {
///         shown.push(f.pts);
///     }
/// }
/// shown.extend(reorder.flush().map(|f| f.pts));
/// assert_eq!(shown, [1, 2, 3, 4]);
/// ```
#[derive(Debug, Clone)]
pub struct FrameReorder {
    /// Waiting frames, in PTS order.
    pending: VecDeque<Frame>,
    /// Frames ready to be popped, in PTS order.
    ready: VecDeque<Frame>,
    max_depth: usize,
    /// The largest DTS seen in the current time base.
    max_dts: Option<u64>,
    discarded: u64,
}

impl FrameReorder {
    /// A reorder buffer holding at most `max_depth` frames (at least 1).
    /// The deepest reordering in H.264 and H.265 is 16 frames.
    pub fn new(max_depth: usize) -> Self {
        FrameReorder {
            pending: VecDeque::new(),
            ready: VecDeque::new(),
            max_depth: max_depth.max(1),
            max_dts: None,
            discarded: 0,
        }
    }

    /// Frames waiting for their turn (not counting those ready to pop).
    pub fn len(&self) -> usize {
        self.pending.len()
    }

    /// Nothing waiting and nothing ready.
    pub fn is_empty(&self) -> bool {
        self.pending.is_empty() && self.ready.is_empty()
    }

    /// Frames of an old time base discarded at a discontinuity.
    pub fn discarded(&self) -> u64 {
        self.discarded
    }

    /// Add a frame, in decode order.
    pub fn push(&mut self, frame: Frame) {
        if frame.discontinuity {
            self.discarded += self.pending.len() as u64;
            self.pending.clear();
            self.max_dts = None;
        }
        let dts = frame.dts.unwrap_or(frame.pts);
        if self.max_dts.map_or(true, |m| pts_delta(dts, m) > 0) {
            self.max_dts = Some(dts);
        }
        let at = self
            .pending
            .iter()
            .rposition(|f| pts_delta(f.pts, frame.pts) <= 0)
            .map_or(0, |i| i + 1);
        self.pending.insert(at, frame);
        let max_dts = self.max_dts.unwrap_or(dts);
        while let Some(f) = self.pending.front() {
            if pts_delta(f.pts, max_dts) <= 0 || self.pending.len() > self.max_depth {
                let f = self.pending.pop_front().expect("front exists");
                self.ready.push_back(f);
            } else {
                break;
            }
        }
    }

    /// The next frame in presentation order, once it is safe.
    pub fn pop(&mut self) -> Option<Frame> {
        self.ready.pop_front()
    }

    /// Release every frame (end of input, or the stream changed), in
    /// presentation order.
    pub fn flush(&mut self) -> impl Iterator<Item = Frame> + '_ {
        self.ready.extend(self.pending.drain(..));
        self.max_dts = None;
        self.ready.drain(..)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PTS_WRAP;

    fn run(r: &mut FrameReorder, frames: &[(u64, Option<u64>)]) -> Vec<u64> {
        let mut out = Vec::new();
        for &(pts, dts) in frames {
            let mut f = Frame::new(0x100, pts);
            f.dts = dts;
            r.push(f);
            while let Some(f) = r.pop() {
                out.push(f.pts);
            }
        }
        out
    }

    #[test]
    fn b_frames_come_out_in_presentation_order_as_soon_as_safe() {
        // I P B B P B B with PTS = DTS + 2 frames, across the PTS wrap.
        let base = PTS_WRAP - 4 * 3003;
        let t = |n: u64| (base + n * 3003) % PTS_WRAP;
        let decode = [(0u64, 0u64), (3, 1), (1, 2), (2, 3), (6, 4), (4, 5), (5, 6)];
        let frames: Vec<(u64, Option<u64>)> = decode
            .iter()
            .map(|&(p, d)| (t(p + 1), Some(t(d))))
            .collect();
        let mut r = FrameReorder::new(16);
        let mut out = run(&mut r, &frames);
        // I0 (pts 1) is safe once DTS 1 is seen, B1 on its own DTS 2, ...;
        // only the last P frame waits for a later DTS.
        assert_eq!(out, (1..=6).map(t).collect::<Vec<_>>());
        out.extend(r.flush().map(|f| f.pts));
        assert_eq!(out, (1..=7).map(t).collect::<Vec<_>>());
        assert!(r.is_empty());
    }

    #[test]
    fn frames_without_dts_and_the_depth_bound() {
        let mut r = FrameReorder::new(16);
        assert_eq!(run(&mut r, &[(10, None), (20, None)]), [10, 20]);
        // Timestamps that never become safe: the depth bound releases them.
        let mut r = FrameReorder::new(2);
        let out = run(
            &mut r,
            &[
                (100, Some(0)),
                (200, Some(1)),
                (300, Some(2)),
                (50, Some(3)),
            ],
        );
        assert_eq!(out, [100, 50]);
        assert_eq!(r.len(), 2);
    }

    #[test]
    fn a_discontinuity_discards_the_old_time_base() {
        let mut r = FrameReorder::new(16);
        assert_eq!(
            run(&mut r, &[(1000, Some(900)), (5000, Some(950))]),
            Vec::<u64>::new()
        );
        let mut f = Frame::new(0x100, 7);
        f.dts = Some(5);
        f.discontinuity = true;
        r.push(f);
        assert_eq!(r.pop(), None);
        assert_eq!((r.len(), r.discarded()), (1, 2));
        assert_eq!(r.flush().map(|f| f.pts).collect::<Vec<_>>(), [7]);
    }
}
