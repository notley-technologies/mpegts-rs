//! Builders for synthetic transport streams.
#![allow(dead_code)]

use mpegts::crc32_mpeg2;
use std::collections::HashMap;

pub const H264: u8 = 0x1B;
pub const H265: u8 = 0x24;
pub const AAC: u8 = 0x0F;
pub const PRIVATE: u8 = 0x06;
pub const METADATA: u8 = 0x15;
pub const KLV_KEY: [u8; 16] = [
    0x06, 0x0E, 0x2B, 0x34, 0x02, 0x0B, 0x01, 0x01, 0x0E, 0x01, 0x03, 0x01, 0x01, 0x00, 0x00, 0x00,
];

/// Long-form section with CRC.
pub fn section(table_id: u8, ext: u16, version: u8, body: &[u8]) -> Vec<u8> {
    section_full(table_id, ext, version, true, 0, 0, body)
}

pub fn section_full(table_id: u8, ext: u16, version: u8, current: bool, number: u8, last: u8, body: &[u8]) -> Vec<u8> {
    let len = 5 + body.len() + 4;
    let mut s = vec![table_id, 0xB0 | (len >> 8) as u8, len as u8];
    s.extend_from_slice(&ext.to_be_bytes());
    s.push(0xC0 | (version << 1) | current as u8);
    s.extend_from_slice(&[number, last]);
    s.extend_from_slice(body);
    let crc = crc32_mpeg2(&s);
    s.extend_from_slice(&crc.to_be_bytes());
    s
}

pub fn pat(tsid: u16, version: u8, programs: &[(u16, u16)]) -> Vec<u8> {
    let mut body = Vec::new();
    for &(n, pid) in programs {
        body.extend_from_slice(&n.to_be_bytes());
        body.extend_from_slice(&(0xE000 | pid).to_be_bytes());
    }
    section(0x00, tsid, version, &body)
}

pub fn pat_body(programs: &[(u16, u16)]) -> Vec<u8> {
    let mut body = Vec::new();
    for &(n, pid) in programs {
        body.extend_from_slice(&n.to_be_bytes());
        body.extend_from_slice(&(0xE000 | pid).to_be_bytes());
    }
    body
}

pub fn pmt(program: u16, version: u8, pcr_pid: u16, prog_desc: &[u8], streams: &[(u8, u16, Vec<u8>)]) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&(0xE000 | pcr_pid).to_be_bytes());
    body.extend_from_slice(&(0xF000 | prog_desc.len() as u16).to_be_bytes());
    body.extend_from_slice(prog_desc);
    for (st, pid, d) in streams {
        body.push(*st);
        body.extend_from_slice(&(0xE000 | pid).to_be_bytes());
        body.extend_from_slice(&(0xF000 | d.len() as u16).to_be_bytes());
        body.extend_from_slice(d);
    }
    section(0x02, program, version, &body)
}

pub fn registration(id: &[u8; 4]) -> Vec<u8> {
    let mut d = vec![0x05, 4];
    d.extend_from_slice(id);
    d
}

pub fn language(code: &[u8; 3]) -> Vec<u8> {
    vec![0x0A, 4, code[0], code[1], code[2], 0]
}

/// metadata_descriptor with application format 0x0100 and format id.
pub fn metadata_descriptor(format_id: &[u8; 4]) -> Vec<u8> {
    let mut b = vec![0x01, 0x00, 0xFF];
    b.extend_from_slice(format_id);
    b.extend_from_slice(&[0x00, 0x0F]); // service id, flags
    let mut d = vec![0x26, b.len() as u8];
    d.extend(b);
    d
}

#[derive(Default, Clone, Copy)]
pub struct Opts {
    pub pusi: bool,
    pub tei: bool,
    pub scrambling: u8,
    pub discontinuity: bool,
    pub pcr: Option<u64>,
    /// Force this continuity counter instead of the next one.
    pub cc: Option<u8>,
    /// Adaptation field only.
    pub no_payload: bool,
}

#[derive(Default)]
pub struct Ts {
    pub out: Vec<u8>,
    cc: HashMap<u16, u8>,
}

impl Ts {
    pub fn new() -> Self {
        Self::default()
    }

    fn next_cc(&mut self, pid: u16) -> u8 {
        let c = self.cc.entry(pid).or_insert(15);
        *c = (*c + 1) & 0xF;
        *c
    }

    /// One packet; payloads shorter than the room left are padded with an
    /// adaptation field (PES) or 0xFF stuffing (PSI).
    pub fn packet_with(&mut self, pid: u16, o: Opts, payload: &[u8], psi: bool) {
        let cc = match o.cc {
            Some(c) => c,
            None if o.no_payload => *self.cc.get(&pid).unwrap_or(&0),
            None => self.next_cc(pid),
        };
        let mut p = vec![
            0x47,
            (o.tei as u8) << 7 | (o.pusi as u8) << 6 | (pid >> 8) as u8,
            pid as u8,
        ];
        let mut af = Vec::new();
        if o.pcr.is_some() || o.discontinuity {
            af.push((o.discontinuity as u8) << 7 | (o.pcr.is_some() as u8) << 4);
        }
        if let Some(pcr) = o.pcr {
            let base = pcr / 300;
            let ext = pcr % 300;
            af.extend_from_slice(&[
                (base >> 25) as u8,
                (base >> 17) as u8,
                (base >> 9) as u8,
                (base >> 1) as u8,
                ((base & 1) << 7) as u8 | 0x7E | (ext >> 8) as u8,
                ext as u8,
            ]);
        }
        let room = 184 - if af.is_empty() { 0 } else { 1 + af.len() };
        assert!(payload.len() <= room);
        let short = room - payload.len();
        let scr = o.scrambling << 6;
        if o.no_payload {
            af.resize(183, 0xFF);
            if af[0] == 0xFF {
                af[0] = 0;
            }
            p.push(scr | 0x20 | cc);
            p.push(183);
            p.extend(af);
        } else if !af.is_empty() || (short > 0 && !psi) {
            if !af.is_empty() {
                af.extend(std::iter::repeat(0xFF).take(short));
                p.push(scr | 0x30 | cc);
                p.push(af.len() as u8);
                p.extend(af);
            } else {
                // `short` bytes of adaptation field: length byte + rest.
                p.push(scr | 0x30 | cc);
                p.push((short - 1) as u8);
                if short > 1 {
                    p.push(0x00);
                    p.extend(std::iter::repeat(0xFF).take(short - 2));
                }
            }
        } else {
            p.push(scr | 0x10 | cc);
        }
        p.extend_from_slice(payload);
        p.resize(188, 0xFF);
        assert_eq!(p.len(), 188);
        self.out.extend(p);
    }

    fn packet(&mut self, pid: u16, pusi: bool, payload: &[u8], psi: bool, pcr: Option<u64>) {
        let o = Opts { pusi, pcr, no_payload: pcr.is_some() && payload.is_empty(), ..Opts::default() };
        self.packet_with(pid, o, payload, psi);
    }

    /// Payload bytes already including any pointer_field.
    pub fn psi_raw(&mut self, pid: u16, pusi: bool, payload: &[u8]) {
        self.packet_with(pid, Opts { pusi, ..Opts::default() }, payload, true);
    }

    pub fn section(&mut self, pid: u16, sec: &[u8]) {
        let mut data = vec![0u8]; // pointer_field
        data.extend_from_slice(sec);
        for (i, chunk) in data.chunks(184).enumerate() {
            self.packet(pid, i == 0, chunk, true, None);
        }
    }

    pub fn pes(&mut self, pid: u16, stream_id: u8, pts: Option<u64>, es: &[u8]) {
        self.pes_with(pid, stream_id, pts, None, Opts::default(), es);
    }

    pub fn pes_with(&mut self, pid: u16, stream_id: u8, pts: Option<u64>, dts: Option<u64>, o: Opts, es: &[u8]) {
        let pes = pes_bytes(stream_id, pts, dts, es);
        for (i, chunk) in pes.chunks(184).enumerate() {
            self.packet_with(pid, Opts { pusi: i == 0, ..o }, chunk, false);
        }
    }

    pub fn pcr(&mut self, pid: u16, pcr: u64) {
        self.packet(pid, false, &[], false, Some(pcr));
    }

    pub fn null(&mut self) {
        self.packet(0x1FFF, false, &[0xFF; 184], true, None);
    }
}

fn timestamp(prefix: u8, t: u64) -> [u8; 5] {
    [
        prefix << 4 | ((t >> 29) & 0x0E) as u8 | 1,
        (t >> 22) as u8,
        ((t >> 14) & 0xFE) as u8 | 1,
        (t >> 7) as u8,
        ((t << 1) & 0xFE) as u8 | 1,
    ]
}

pub fn pes_bytes(stream_id: u8, pts: Option<u64>, dts: Option<u64>, es: &[u8]) -> Vec<u8> {
    let mut pes = vec![0, 0, 1, stream_id, 0, 0];
    match (pts, dts) {
        (Some(p), Some(d)) => {
            pes.extend_from_slice(&[0x80, 0xC0, 10]);
            pes.extend_from_slice(&timestamp(3, p));
            pes.extend_from_slice(&timestamp(1, d));
        }
        (Some(p), None) => {
            pes.extend_from_slice(&[0x80, 0x80, 5]);
            pes.extend_from_slice(&timestamp(2, p));
        }
        _ => pes.extend_from_slice(&[0x80, 0x00, 0]),
    }
    pes.extend_from_slice(es);
    // PES_packet_length, as encoders set it for everything but video.
    if let Ok(len) = u16::try_from(pes.len() - 6) {
        pes[4..6].copy_from_slice(&len.to_be_bytes());
    }
    pes
}

/// Small deterministic PRNG (xorshift64*).
pub struct Rng(pub u64);

impl Rng {
    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    pub fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}
