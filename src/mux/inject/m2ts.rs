//! TP_extra_header of the records written with [`RecordFormat::M2ts`]
//! (MUXING_PLAN 6.6): input records keep theirs; a packet the injector
//! adds takes the copy_permission_indicator of the next input record and
//! an arrival_time_stamp interpolated between its neighbours.
//!
//! [`RecordFormat::M2ts`]: super::RecordFormat::M2ts

use crate::mux::{Packet, PacketSink, Record, M2TS_RECORD_SIZE};
use crate::PACKET_SIZE;

/// arrival_time_stamp: the low 30 bits of the TP_extra_header.
const ATS_MASK: u32 = (1 << 30) - 1;

fn ats(h: [u8; 4]) -> u32 {
    u32::from_be_bytes(h) & ATS_MASK
}

/// A header with `like`'s copy_permission_indicator and `stamp`.
fn header(like: [u8; 4], stamp: u32) -> [u8; 4] {
    ((u32::from_be_bytes(like) & !ATS_MASK) | (stamp & ATS_MASK)).to_be_bytes()
}

fn record(h: [u8; 4], p: &Packet) -> Record {
    let mut r = [0u8; M2TS_RECORD_SIZE];
    r[..4].copy_from_slice(&h);
    r[4..].copy_from_slice(&p[..PACKET_SIZE]);
    r
}

/// Stamps the output records. Packets added in VBR wait in `pending` until
/// the input record they precede is known.
#[derive(Debug, Default)]
pub(super) struct Stamper {
    /// The input record being processed, and the one before.
    cur: [u8; 4],
    prev: Option<[u8; 4]>,
    seen: bool,
    /// The current record went out (as itself or in its place).
    cur_out: bool,
    pending: Vec<Packet>,
    /// Last stamp given after the end of input.
    tail: Option<u32>,
}

impl Stamper {
    pub fn new() -> Self {
        Stamper::default()
    }

    /// The next input record.
    pub fn begin(&mut self, rec: [u8; 4]) {
        if self.seen {
            self.prev = Some(self.cur);
        }
        self.cur = rec;
        self.seen = true;
        self.cur_out = false;
        self.tail = None;
    }

    /// A packet the injector adds before the current record's slot (VBR).
    pub fn inserted(&mut self, p: &Packet) {
        self.pending.push(*p);
    }

    /// The current record's slot: the packets waiting before it, then `p`
    /// with the record's header.
    pub fn slot<S: PacketSink + ?Sized>(&mut self, p: &Packet, sink: &mut S) {
        self.flush(sink, true);
        sink.record(&record(self.cur, p));
        self.cur_out = true;
    }

    /// The current input packet is processed: when its record did not go
    /// out (the injector consumed it), the packets added before it end on
    /// its stamp.
    pub fn end_packet<S: PacketSink + ?Sized>(&mut self, sink: &mut S) {
        if !self.cur_out {
            self.flush(sink, false);
        }
    }

    /// Spread the pending packets evenly between the previous and current
    /// records' stamps (modulo 2^30), the last one on the current stamp
    /// when the current record did not go out.
    fn flush<S: PacketSink + ?Sized>(&mut self, sink: &mut S, before_slot: bool) {
        let k = self.pending.len() as u64;
        if k == 0 {
            return;
        }
        let b = ats(self.cur);
        let a = self.prev.map_or(b, ats);
        let delta = u64::from(b.wrapping_sub(a) & ATS_MASK);
        let den = if before_slot { k + 1 } else { k };
        for (i, p) in self.pending.iter().enumerate() {
            let step = (delta * (i as u64 + 1) / den) as u32;
            sink.record(&record(header(self.cur, a.wrapping_add(step)), p));
        }
        self.pending.clear();
    }

    /// No more input: the pending packets continue the spacing of the last
    /// two input records.
    pub fn finish<S: PacketSink + ?Sized>(&mut self, sink: &mut S) {
        if self.pending.is_empty() {
            return;
        }
        let last = ats(self.cur);
        let spacing = self.prev.map_or(0, |p| last.wrapping_sub(ats(p)) & ATS_MASK);
        let mut stamp = self.tail.unwrap_or(last);
        for p in &self.pending {
            stamp = stamp.wrapping_add(spacing) & ATS_MASK;
            sink.record(&record(header(self.cur, stamp), p));
        }
        self.tail = Some(stamp);
        self.pending.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stamps(out: &[u8]) -> Vec<u32> {
        out.chunks(M2TS_RECORD_SIZE).map(|r| u32::from_be_bytes([r[0], r[1], r[2], r[3]])).collect()
    }

    #[test]
    fn added_packets_are_spread_between_their_neighbours() {
        let p = [0x47u8; PACKET_SIZE];
        let mut s = Stamper::new();
        let mut out = Vec::new();
        // copy_permission_indicator 01 on the second record.
        s.begin(1000u32.to_be_bytes());
        s.slot(&p, &mut out);
        s.end_packet(&mut out);
        s.begin((0x4000_0000u32 | 1300).to_be_bytes());
        s.inserted(&p);
        s.inserted(&p);
        s.slot(&p, &mut out);
        // A consumed record: what was added before it ends on its stamp.
        s.begin(1500u32.to_be_bytes());
        s.inserted(&p);
        s.end_packet(&mut out);
        // Across the wrap of the 30-bit stamp.
        s.begin((ATS_MASK - 49).to_be_bytes());
        s.slot(&p, &mut out);
        s.begin(50u32.to_be_bytes());
        s.inserted(&p);
        s.slot(&p, &mut out);
        s.inserted(&p);
        s.finish(&mut out);
        assert_eq!(
            stamps(&out),
            [1000, 0x4000_0000 | 1100, 0x4000_0000 | 1200, 0x4000_0000 | 1300, 1500, ATS_MASK - 49, 0, 50, 150]
        );
    }
}
