//! The program clock time of every TS packet.
//!
//! ISO/IEC 13818-1 2.4.2.2 (R-ISO13818-043) defines the arrival time of
//! every byte between two PCRs of a program by linear interpolation: the
//! transport rate of an interval is its byte count divided by the PCR
//! difference. [`PcrTimeline`] applies that to whole packets, so a
//! pass-through tool can tell when each input packet is due, and an
//! injector can place what it inserts on the same clock.

use crate::demuxer::PACKET_SIZE;
use crate::packet::{pcr_delta, PacketHeader, PCR_WRAP};
use crate::report::PCR_HZ;
use std::collections::VecDeque;

/// Default for [`PcrTimeline::with_max_held`]: 20,000 packets, about
/// 3.8 MB, which covers a PCR interval at over 300 Mbit/s even at the
/// 100 ms the standard allows (R-ISO13818-045).
pub const DEFAULT_MAX_HELD: usize = 20_000;

/// The longest PCR gap trusted for interpolation. ISO/IEC 13818-1 2.7.2
/// allows at most 100 ms between PCRs (R-ISO13818-045); a gap of a second
/// is a splice, a loop back to the start of a file or lost input, and the
/// previous rate is a better guess than spreading the packets over it.
const MAX_PCR_GAP: i64 = PCR_HZ as i64;

/// A packet released by a [`PcrTimeline`], with its times.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimedPacket {
    /// The packet, unchanged.
    pub packet: [u8; PACKET_SIZE],
    /// Position on a continuous 27 MHz time line that starts at 0 and never
    /// goes backwards, whatever the stream's own clock does (wraps,
    /// discontinuities, jumps). Use it to pace output.
    pub position: u64,
    /// The program clock (27 MHz, wrapping at [`PCR_WRAP`](crate::PCR_WRAP))
    /// at this packet: exactly the PCR on PCR packets, interpolated or
    /// extrapolated on the others. Compare it with PTS values (PTS x 300).
    /// `None` until the first PCR.
    pub clock: Option<u64>,
}

/// Gives each TS packet its time on the program clock.
///
/// Packets are held until the next PCR on the PCR PID arrives; the packets
/// of that interval are then spread evenly between the two PCRs, which is
/// the T-STD's own model (ISO/IEC 13818-1 2.4.2.2). A PCR gap of a second
/// or more, a backwards step or a discontinuity_indicator (2.4.3.5) keeps
/// the previous rate instead, so [`TimedPacket::position`] stays continuous
/// while [`TimedPacket::clock`] follows the stream's own PCR values.
///
/// Memory is bounded: when [`max_held`](Self::with_max_held) packets are
/// waiting without a PCR, they are released at the last known rate.
/// Arithmetic is in integer 27 MHz ticks, so long runs do not drift.
///
/// ```
/// use mpegts::PcrTimeline;
///
/// let mut timeline = PcrTimeline::new(None, 4_000_000);
/// let mut timed = Vec::new();
/// # let input: Vec<[u8; 188]> = Vec::new();
/// for packet in &input {
///     timeline.push(packet, &mut |t| timed.push(t));
/// }
/// timeline.flush(&mut |t| timed.push(t));
/// ```
#[derive(Debug, Clone)]
pub struct PcrTimeline {
    /// The PID whose PCR is followed; `None` until the first PCR, when it
    /// locks onto the PID carrying it.
    pcr_pid: Option<u16>,
    held: VecDeque<[u8; PACKET_SIZE]>,
    max_held: usize,
    /// A PCR has been seen.
    started: bool,
    /// Program clock at the last released packet.
    last_clock: Option<u64>,
    /// Position of the last released packet.
    position: u64,
    /// Ticks per packet as a fraction, from the last trusted PCR interval
    /// (or the initial bitrate).
    rate_ticks: u64,
    rate_packets: u64,
}

impl PcrTimeline {
    /// A timeline following the PCR on `pcr_pid` (`None`: the first PID
    /// that carries one). `bitrate` (bits per second of the whole
    /// transport stream) sets the rate used until two PCRs have been seen.
    pub fn new(pcr_pid: Option<u16>, bitrate: u64) -> Self {
        PcrTimeline {
            pcr_pid,
            held: VecDeque::new(),
            max_held: DEFAULT_MAX_HELD,
            started: false,
            last_clock: None,
            position: 0,
            rate_ticks: (PACKET_SIZE as u64 * 8) * PCR_HZ,
            rate_packets: bitrate.max(1),
        }
    }

    /// Hold at most `max_held` packets (at least 1) while waiting for a
    /// PCR. Default [`DEFAULT_MAX_HELD`].
    pub fn with_max_held(mut self, max_held: usize) -> Self {
        self.max_held = max_held.max(1);
        self
    }

    /// The PID whose PCR is followed.
    pub fn pcr_pid(&self) -> Option<u16> {
        self.pcr_pid
    }

    /// Follow another PID's PCR, for example after the program's PMT
    /// changed its PCR_PID. The next PCR on it starts a new interval.
    pub fn set_pcr_pid(&mut self, pid: u16) {
        if self.pcr_pid != Some(pid) {
            self.pcr_pid = Some(pid);
            self.last_clock = None;
        }
    }

    /// Packets waiting for the next PCR.
    pub fn held(&self) -> usize {
        self.held.len()
    }

    /// Add a packet. When it ends a PCR interval (or fills the hold
    /// limit), the waiting packets are passed to `out` in order, with their
    /// times.
    pub fn push(&mut self, packet: &[u8; PACKET_SIZE], out: &mut impl FnMut(TimedPacket)) {
        // A packet with transport_error_indicator set may have a damaged
        // header (2.4.3.3), so its PCR is not trusted.
        let pcr = match PacketHeader::parse(packet) {
            Ok(h) if !h.transport_error => h.pcr().and_then(|pcr| match self.pcr_pid {
                Some(pid) if pid != h.pid => None,
                _ => {
                    self.pcr_pid = Some(h.pid);
                    Some((pcr, h.discontinuity()))
                }
            }),
            _ => None,
        };
        self.held.push_back(*packet);
        if pcr.is_some() || self.held.len() >= self.max_held {
            self.release(pcr, out);
        }
    }

    /// Release every waiting packet at the last known rate (end of input,
    /// or no PCR is coming).
    pub fn flush(&mut self, out: &mut impl FnMut(TimedPacket)) {
        if !self.held.is_empty() {
            self.release(None, out);
        }
    }

    /// Ticks that `n` packets take at the current rate, rounded.
    fn extrapolate(&self, n: u64) -> u64 {
        let ticks = u128::from(self.rate_ticks) * u128::from(n);
        let den = u128::from(self.rate_packets);
        ((ticks + den / 2) / den).min(u128::from(u64::MAX)) as u64
    }

    fn release(&mut self, pcr: Option<(u64, bool)>, out: &mut impl FnMut(TimedPacket)) {
        let n = self.held.len() as u64;
        let span = match (pcr, self.last_clock) {
            // The first PCR: what came before it is not timed by anything,
            // so it goes out at once.
            (Some(_), _) if !self.started => 0,
            (Some((pcr, discontinuity)), Some(last)) => {
                let d = pcr_delta(pcr, last);
                if !discontinuity && d > 0 && d < MAX_PCR_GAP {
                    self.rate_ticks = d as u64;
                    self.rate_packets = n;
                    d as u64
                } else {
                    self.extrapolate(n)
                }
            }
            _ => self.extrapolate(n),
        };
        let mut k = 0u64;
        while let Some(packet) = self.held.pop_front() {
            k += 1;
            let step = (u128::from(span) * u128::from(k) / u128::from(n)) as u64;
            let clock = match pcr {
                Some((pcr, _)) if k == n => Some(pcr % PCR_WRAP),
                _ => self.last_clock.map(|c| (c + step % PCR_WRAP) % PCR_WRAP),
            };
            out(TimedPacket { packet, position: self.position.saturating_add(step), clock });
        }
        self.position = self.position.saturating_add(span);
        self.last_clock = match pcr {
            Some((pcr, _)) => Some(pcr % PCR_WRAP),
            None => self.last_clock.map(|c| (c + span % PCR_WRAP) % PCR_WRAP),
        };
        self.started |= pcr.is_some();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn packet(pid: u16, pcr: Option<u64>, discontinuity: bool) -> [u8; PACKET_SIZE] {
        let mut p = [0xFF; PACKET_SIZE];
        p[0] = 0x47;
        p[1] = (pid >> 8) as u8;
        p[2] = pid as u8;
        if pcr.is_none() && !discontinuity {
            p[3] = 0x10;
            return p;
        }
        p[3] = 0x30;
        p[4] = 7;
        p[5] = u8::from(discontinuity) << 7 | u8::from(pcr.is_some()) << 4;
        if let Some(pcr) = pcr {
            let (base, ext) = (pcr / 300, pcr % 300);
            p[6] = (base >> 25) as u8;
            p[7] = (base >> 17) as u8;
            p[8] = (base >> 9) as u8;
            p[9] = (base >> 1) as u8;
            p[10] = ((base << 7) as u8 & 0x80) | 0x7E | (ext >> 8) as u8;
            p[11] = ext as u8;
        }
        p
    }

    fn run(t: &mut PcrTimeline, packets: &[[u8; PACKET_SIZE]]) -> Vec<(u64, Option<u64>)> {
        let mut out = Vec::new();
        for p in packets {
            t.push(p, &mut |tp| out.push((tp.position, tp.clock)));
        }
        out
    }

    #[test]
    fn spreads_packets_between_pcrs_and_survives_a_jump() {
        let mut t = PcrTimeline::new(None, 1_000_000);
        let mut input = vec![packet(0x100, Some(1_000_000), false)];
        input.extend([packet(0x101, None, false); 3]);
        input.push(packet(0x100, Some(1_400_000), false));
        let out = run(&mut t, &input);
        assert_eq!(
            out,
            [
                (0, Some(1_000_000)),
                (100_000, Some(1_100_000)),
                (200_000, Some(1_200_000)),
                (300_000, Some(1_300_000)),
                (400_000, Some(1_400_000))
            ]
        );
        assert_eq!(t.pcr_pid(), Some(0x100));
        // The clock jumps back: positions keep going at the same rate, the
        // clock follows the stream.
        let out = run(&mut t, &[packet(0x101, None, false), packet(0x100, Some(5_000), false)]);
        assert_eq!(out, [(500_000, Some(1_500_000)), (600_000, Some(5_000))]);
        // A discontinuity_indicator: same, even for a forward step.
        let out = run(&mut t, &[packet(0x101, None, false), packet(0x100, Some(50_000), true)]);
        assert_eq!(out, [(700_000, Some(105_000)), (800_000, Some(50_000))]);
        // A gap of a second or more: same.
        let out = run(&mut t, &[packet(0x100, Some(50_000 + 27_000_000), false)]);
        assert_eq!(out, [(900_000, Some(27_050_000))]);
    }

    #[test]
    fn follows_one_pid_and_ignores_damaged_packets() {
        let mut t = PcrTimeline::new(Some(0x200), 1_000_000);
        let mut damaged = packet(0x200, Some(9_000), false);
        damaged[1] |= 0x80;
        let out = run(&mut t, &[packet(0x100, Some(1_000), false), damaged, packet(0x200, Some(2_000), false)]);
        assert_eq!(out.len(), 3);
        assert!(out.iter().all(|&(pos, _)| pos == 0));
        assert_eq!(out[2].1, Some(2_000));
        assert_eq!(out[0].1, None, "no clock before the first PCR");
    }

    #[test]
    fn interpolation_is_exact_across_the_pcr_wrap() {
        let mut t = PcrTimeline::new(None, 1_000_000);
        let start = PCR_WRAP - 150;
        let out = run(
            &mut t,
            &[packet(0x100, Some(start), false), packet(0x101, None, false), packet(0x100, Some(150), false)],
        );
        assert_eq!(out[1], (150, Some(0)));
        assert_eq!(out[2], (300, Some(150)));
    }

    #[test]
    fn hold_limit_and_flush_use_the_last_rate() {
        let mut t = PcrTimeline::new(None, 1_000_000).with_max_held(2);
        let out = run(&mut t, &[packet(0x100, Some(0), false), packet(0x101, None, false), packet(0x100, Some(1_000), false)]);
        assert_eq!(out.last(), Some(&(1_000, Some(1_000))));
        // Two packets without a PCR: released at 500 ticks per packet.
        let out = run(&mut t, &[packet(0x101, None, false), packet(0x101, None, false)]);
        assert_eq!(out, [(1_500, Some(1_500)), (2_000, Some(2_000))]);
        assert_eq!(t.held(), 0);
        let mut out = Vec::new();
        t.push(&packet(0x101, None, false), &mut |tp| out.push(tp.position));
        assert!(out.is_empty());
        t.flush(&mut |tp| out.push(tp.position));
        assert_eq!(out, [2_500]);
    }

    #[test]
    fn initial_bitrate_times_packets_before_two_pcrs() {
        // 188 * 8 bits at 1,504,000 bit/s = 1 ms = 27,000 ticks per packet.
        let mut t = PcrTimeline::new(None, 1_504_000).with_max_held(3);
        let out = run(&mut t, &[packet(0x101, None, false); 3]);
        assert_eq!(out, [(27_000, None), (54_000, None), (81_000, None)]);
    }

    #[test]
    fn switching_pcr_pid_starts_a_new_interval() {
        let mut t = PcrTimeline::new(None, 1_000_000);
        run(&mut t, &[packet(0x100, Some(0), false), packet(0x100, Some(1_000), false)]);
        t.set_pcr_pid(0x300);
        // PCRs on the old PID no longer count; the new PID's first PCR has
        // nothing to interpolate from, so the last rate (1,000 ticks per
        // packet) applies and the old clock is no longer extrapolated.
        let out = run(&mut t, &[packet(0x100, Some(2_000), false), packet(0x300, Some(90_000_000), false)]);
        assert_eq!(out, [(2_000, None), (3_000, Some(90_000_000))]);
    }
}
