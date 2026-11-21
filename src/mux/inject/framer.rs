//! Packet framing of byte input in any chunking, with resynchronisation
//! after corrupt or missing bytes: 188-byte packets, or 192-byte M2TS
//! records whose sync byte follows a 4-byte TP_extra_header.

use crate::{PACKET_SIZE, SYNC_BYTE};

/// Sync bytes that must line up at the record stride after a candidate
/// before the framer trusts it (the demuxer's default, `Config::sync_confirmations`).
const CONFIRMATIONS: usize = 2;

#[derive(Debug)]
pub(super) struct Framer {
    /// Bytes per unit: 188, or 192 for M2TS.
    stride: usize,
    /// A partial unit (locked) or unresolved bytes (hunting).
    carry: Vec<u8>,
    locked: bool,
    pub bytes_skipped: u64,
    pub sync_losses: u64,
}

impl Default for Framer {
    fn default() -> Self {
        Framer::new(PACKET_SIZE)
    }
}

impl Framer {
    /// A framer for units of `stride` bytes (188 or 192), each ending with
    /// a 188-byte packet.
    pub fn new(stride: usize) -> Self {
        Framer {
            stride: stride.max(PACKET_SIZE),
            carry: Vec::new(),
            locked: false,
            bytes_skipped: 0,
            sync_losses: 0,
        }
    }

    /// Offset of the sync byte in a unit.
    fn sync_at(&self) -> usize {
        self.stride - PACKET_SIZE
    }

    /// Frame `data`, calling `f` for each whole unit (`stride` bytes) in
    /// order until it returns false.
    pub fn push(&mut self, mut data: &[u8], f: &mut dyn FnMut(&[u8]) -> bool) {
        if self.locked && !self.carry.is_empty() {
            let have = self.carry.len();
            let need = self.stride - have;
            if data.len() < need {
                self.carry.extend_from_slice(data);
                return;
            }
            self.carry.extend_from_slice(&data[..need]);
            if self.carry[self.sync_at()] == SYNC_BYTE {
                data = &data[need..];
                let unit = std::mem::take(&mut self.carry);
                let go_on = f(&unit);
                self.carry = unit;
                self.carry.clear();
                if !go_on {
                    return;
                }
            } else {
                // An M2TS tail too short to show its sync byte when it was
                // kept: hunt from its start.
                self.carry.truncate(have);
                self.lose();
            }
        }
        if self.locked {
            match self.run(data, f) {
                Ok(()) => return,
                Err(at) => data = &data[at..],
            }
        }
        // Hunting: join what is left from before with the new bytes.
        let mut buf = std::mem::take(&mut self.carry);
        buf.extend_from_slice(data);
        self.hunt(buf, false, f);
    }

    /// End of input: whole units still waiting for confirmation are
    /// accepted if they line up; a partial unit is discarded.
    pub fn finish(&mut self, f: &mut dyn FnMut(&[u8]) -> bool) {
        let buf = std::mem::take(&mut self.carry);
        if self.locked {
            self.bytes_skipped += buf.len() as u64;
        } else {
            self.hunt(buf, true, f);
            self.bytes_skipped += self.carry.len() as u64;
            self.carry.clear();
        }
        self.locked = false;
    }

    /// Aligned units. Returns the offset where sync was lost; keeps a
    /// short aligned tail.
    fn run(&mut self, data: &[u8], f: &mut dyn FnMut(&[u8]) -> bool) -> Result<(), usize> {
        let (stride, sync) = (self.stride, self.sync_at());
        let mut at = 0;
        for unit in data.chunks_exact(stride) {
            if unit[sync] != SYNC_BYTE {
                self.lose();
                return Err(at);
            }
            if !f(unit) {
                return Ok(());
            }
            at += stride;
        }
        if at < data.len() {
            // The tail's sync byte is checked now when it is there, else
            // when the unit is complete.
            if data.len() - at > sync && data[at + sync] != SYNC_BYTE {
                self.lose();
                return Err(at);
            }
            self.carry.extend_from_slice(&data[at..]);
        }
        Ok(())
    }

    fn lose(&mut self) {
        self.locked = false;
        self.sync_losses += 1;
    }

    fn hunt(&mut self, buf: Vec<u8>, eof: bool, f: &mut dyn FnMut(&[u8]) -> bool) {
        let (stride, sync) = (self.stride, self.sync_at());
        // Start of the bytes not yet skipped or framed.
        let mut base = 0;
        'search: loop {
            // A sync byte with a whole TP_extra_header before it.
            let mut i = base + sync;
            while let Some(off) = buf.get(i..).and_then(|b| b.iter().position(|&b| b == SYNC_BYTE)) {
                let c = i + off;
                let mut confirmed = 0;
                let mut mismatch = false;
                for k in 1..=CONFIRMATIONS {
                    match buf.get(c + k * stride) {
                        Some(&SYNC_BYTE) => confirmed += 1,
                        Some(_) => {
                            mismatch = true;
                            break;
                        }
                        None => break,
                    }
                }
                let u = c - sync;
                if !mismatch && (confirmed == CONFIRMATIONS || (eof && u + stride <= buf.len())) {
                    self.bytes_skipped += (u - base) as u64;
                    self.locked = true;
                    match self.run(&buf[u..], f) {
                        Ok(()) => return,
                        Err(at) => {
                            base = u + at;
                            continue 'search;
                        }
                    }
                }
                if !mismatch && !eof {
                    // Not enough bytes yet to confirm this candidate.
                    self.bytes_skipped += (u - base) as u64;
                    self.carry = buf[u..].to_vec();
                    return;
                }
                i = c + 1;
            }
            // No unit found: the last bytes may still be the header of a
            // record whose sync byte has not arrived yet.
            let keep = buf.len().saturating_sub(sync).max(base).min(buf.len());
            self.bytes_skipped += (keep - base.min(keep)) as u64;
            self.carry = buf[keep..].to_vec();
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn packets(n: usize) -> Vec<u8> {
        let mut v = Vec::new();
        for i in 0..n {
            let mut p = [i as u8; PACKET_SIZE];
            p[0] = SYNC_BYTE;
            v.extend_from_slice(&p);
        }
        v
    }

    fn frame_with(stride: usize, chunks: &[&[u8]]) -> (Vec<u8>, Framer) {
        let mut fr = Framer::new(stride);
        let mut out = Vec::new();
        let at = stride - PACKET_SIZE;
        for c in chunks {
            fr.push(c, &mut |u| {
                assert_eq!(u.len(), stride);
                out.push(u[at + 1]);
                true
            });
        }
        fr.finish(&mut |u| {
            out.push(u[at + 1]);
            true
        });
        (out, fr)
    }

    fn frame(chunks: &[&[u8]]) -> (Vec<u8>, Framer) {
        frame_with(PACKET_SIZE, chunks)
    }

    #[test]
    fn any_chunking_gives_the_same_packets() {
        let ts = packets(10);
        for size in [1, 7, 187, 188, 189, 1000, 2000] {
            let chunks: Vec<&[u8]> = ts.chunks(size).collect();
            let (out, fr) = frame(&chunks);
            assert_eq!(out, (0..10).collect::<Vec<u8>>(), "chunk {size}");
            assert_eq!((fr.bytes_skipped, fr.sync_losses), (0, 0));
        }
    }

    #[test]
    fn resyncs_after_garbage_and_drops_a_partial_tail() {
        let mut ts = vec![1, 2, 3];
        ts.extend(packets(4));
        ts.extend([0x12, 9, 9]);
        ts.extend(packets(4));
        ts.extend([0x47; 50]);
        let (out, fr) = frame(&[&ts]);
        assert_eq!(out, [0, 1, 2, 3, 0, 1, 2, 3]);
        assert_eq!(fr.sync_losses, 1);
        assert_eq!(fr.bytes_skipped, 3 + 3 + 50);
        // A single packet is accepted at the end of input.
        let (out, _) = frame(&[&packets(1)]);
        assert_eq!(out, [0]);
    }

    /// M2TS: a 4-byte header before each packet.
    fn records(n: usize) -> Vec<u8> {
        let mut v = Vec::new();
        for (i, p) in packets(n).chunks(PACKET_SIZE).enumerate() {
            v.extend_from_slice(&[0x40, 0, 0, i as u8]);
            v.extend_from_slice(p);
        }
        v
    }

    #[test]
    fn m2ts_records_in_any_chunking() {
        let m2ts = records(10);
        for size in [1, 3, 5, 191, 192, 193, 1000] {
            let chunks: Vec<&[u8]> = m2ts.chunks(size).collect();
            let (out, fr) = frame_with(192, &chunks);
            assert_eq!(out, (0..10).collect::<Vec<u8>>(), "chunk {size}");
            assert_eq!((fr.bytes_skipped, fr.sync_losses), (0, 0), "chunk {size}");
        }
        // Garbage in the middle, a record cut short at the start.
        let mut bad = m2ts[100..192 * 4].to_vec();
        bad.extend([1, 2, 3]);
        bad.extend_from_slice(&m2ts[192 * 4..]);
        let (out, fr) = frame_with(192, &[&bad]);
        assert_eq!(out, [1, 2, 3, 4, 5, 6, 7, 8, 9]);
        assert_eq!(fr.sync_losses, 1);
        assert_eq!(fr.bytes_skipped, 92 + 3);
    }
}
