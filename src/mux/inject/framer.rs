//! 188-byte packet framing of byte input in any chunking, with
//! resynchronisation after corrupt or missing bytes.

use crate::{PACKET_SIZE, SYNC_BYTE};

/// Sync bytes that must line up at a 188-byte stride after a candidate
/// before the framer trusts it (the demuxer's default, `Config::sync_confirmations`).
const CONFIRMATIONS: usize = 2;

#[derive(Debug, Default)]
pub(super) struct Framer {
    /// A partial packet (locked) or unresolved bytes (hunting).
    carry: Vec<u8>,
    locked: bool,
    pub bytes_skipped: u64,
    pub sync_losses: u64,
}

impl Framer {
    /// Frame `data`, calling `f` for each whole packet in order until it
    /// returns false.
    pub fn push(&mut self, mut data: &[u8], f: &mut dyn FnMut(&[u8; PACKET_SIZE]) -> bool) {
        if self.locked && !self.carry.is_empty() {
            let need = PACKET_SIZE - self.carry.len();
            if data.len() < need {
                self.carry.extend_from_slice(data);
                return;
            }
            self.carry.extend_from_slice(&data[..need]);
            data = &data[need..];
            let mut p = [0u8; PACKET_SIZE];
            p.copy_from_slice(&self.carry);
            self.carry.clear();
            if !f(&p) {
                return;
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

    /// End of input: whole packets still waiting for confirmation are
    /// accepted if they line up; a partial packet is discarded.
    pub fn finish(&mut self, f: &mut dyn FnMut(&[u8; PACKET_SIZE]) -> bool) {
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

    /// Aligned packets. Returns the offset where sync was lost; keeps a
    /// short aligned tail.
    fn run(
        &mut self,
        data: &[u8],
        f: &mut dyn FnMut(&[u8; PACKET_SIZE]) -> bool,
    ) -> Result<(), usize> {
        let mut at = 0;
        while data.len() - at >= PACKET_SIZE {
            if data[at] != SYNC_BYTE {
                self.lose();
                return Err(at);
            }
            let p: &[u8; PACKET_SIZE] = data[at..at + PACKET_SIZE].try_into().expect("188 bytes");
            if !f(p) {
                return Ok(());
            }
            at += PACKET_SIZE;
        }
        if at < data.len() {
            if data[at] != SYNC_BYTE {
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

    fn hunt(&mut self, buf: Vec<u8>, eof: bool, f: &mut dyn FnMut(&[u8; PACKET_SIZE]) -> bool) {
        // Start of the bytes not yet skipped or framed.
        let mut base = 0;
        'search: loop {
            let mut i = base;
            while let Some(off) = buf[i..].iter().position(|&b| b == SYNC_BYTE) {
                let c = i + off;
                let mut confirmed = 0;
                let mut mismatch = false;
                for k in 1..=CONFIRMATIONS {
                    match buf.get(c + k * PACKET_SIZE) {
                        Some(&SYNC_BYTE) => confirmed += 1,
                        Some(_) => {
                            mismatch = true;
                            break;
                        }
                        None => break,
                    }
                }
                if !mismatch
                    && (confirmed == CONFIRMATIONS || (eof && c + PACKET_SIZE <= buf.len()))
                {
                    self.bytes_skipped += (c - base) as u64;
                    self.locked = true;
                    match self.run(&buf[c..], f) {
                        Ok(()) => return,
                        Err(at) => {
                            base = c + at;
                            continue 'search;
                        }
                    }
                }
                if !mismatch && !eof {
                    // Not enough bytes yet to confirm this candidate.
                    self.bytes_skipped += (c - base) as u64;
                    self.carry = buf[c..].to_vec();
                    return;
                }
                i = c + 1;
            }
            self.bytes_skipped += (buf.len() - base) as u64;
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

    fn frame(chunks: &[&[u8]]) -> (Vec<u8>, Framer) {
        let mut fr = Framer::default();
        let mut out = Vec::new();
        for c in chunks {
            fr.push(c, &mut |p| {
                out.push(p[1]);
                true
            });
        }
        fr.finish(&mut |p| {
            out.push(p[1]);
            true
        });
        (out, fr)
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
}
