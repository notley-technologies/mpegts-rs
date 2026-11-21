//! Fuzz harnesses, shared by the coverage-guided targets in `fuzz/` and the
//! stable-toolchain fuzz test `tests/fuzz.rs`.
//!
//! Each harness accepts any bytes, must never panic, and checks properties
//! that hold for every input:
//!
//! - the report and the extracted data do not depend on how the input is
//!   split into pushes;
//! - every buffer stays within its `Config` limit;
//! - reports and outputs are internally consistent;
//! - the C API returns only documented status codes and never reports an
//!   internal error;
//! - what the write primitives write (feature `mux`) obeys the packet rules
//!   and reads back unchanged;
//! - the injector (feature `mux`) passes every packet it does not own
//!   through unchanged, within its memory limits;
//! - the video NAL unit reader (`mpegts::nal`) stays within its input, and
//!   emulation prevention and SEI rebuilding round-trip.
#![allow(dead_code)]

#[path = "../common/mod.rs"]
pub mod common;

use common::*;
#[cfg(feature = "capi")]
use mpegts::capi::*;
use mpegts::{
    crc32_mpeg2, Codec, Config, Demuxer, Output, Overflow, Report, Select, StreamKind, Subscription,
    SubscriptionId, Unit,
};
#[cfg(feature = "capi")]
use std::ffi::CStr;
use std::hint::black_box;
use std::mem;
#[cfg(feature = "capi")]
use std::ptr;

#[cfg(feature = "mux")]
pub mod check;
pub mod es;
#[cfg(feature = "mux")]
mod inject;
#[cfg(feature = "mux")]
mod mux;
#[cfg(feature = "mux")]
pub mod synth;
// Each fuzz target uses one harness; the rest is unused there.
#[cfg(feature = "mux")]
#[allow(unused_imports)]
pub use inject::{inject, inject_run};
#[cfg(feature = "mux")]
#[allow(unused_imports)]
pub use mux::{mux, mux_run};

/// Most packets one generated stream holds, to keep each run fast.
const MAX_PACKETS: usize = 3000;

// ---------------------------------------------------------------- targets

/// Video elementary stream bytes for `mpegts::nal`: NAL units, user data,
/// emulation prevention and SEI rebuilding, for every codec.
pub fn nal(data: &[u8]) {
    use mpegts::nal::*;
    let codecs = [VideoCodec::H264, VideoCodec::H265, VideoCodec::Mpeg2];
    for codec in codecs {
        let mut found = Vec::new();
        find_user_data(codec, data, &mut found);
        for m in &found {
            let p = m.position;
            assert!(p.start <= p.header && p.header <= p.end && p.end <= data.len(), "{p:?}");
            assert!(m.payload.len() + 16 <= data.len());
        }
    }
    let mut prev_end = 0;
    let mut rebuilt = Vec::new();
    for u in nal_units(data) {
        assert!(u.start >= prev_end && u.start <= u.offset && u.end() <= u.next && u.next <= data.len());
        assert_eq!(&data[u.offset..u.end()], u.data);
        prev_end = u.end();
        for codec in [VideoCodec::H264, VideoCodec::H265] {
            let new = &data[..data.len().min(40)];
            rebuilt.clear();
            if replace_sei_payload(codec, u.data, 0, new, &mut rebuilt) {
                // The replaced message reads back.
                let mut rbsp = Vec::new();
                remove_emulation_prevention(&rebuilt[codec.header_len()..], &mut rbsp);
                let first = sei_messages(&rbsp).next().expect("message 0");
                assert_eq!(first.payload, new);
            }
        }
    }
    // Emulation prevention: no start code emulation, and back again.
    let mut ebsp = Vec::new();
    add_emulation_prevention(data, &mut ebsp);
    for w in ebsp.windows(3) {
        assert!(!(w[0] == 0 && w[1] == 0 && w[2] <= 2), "start code emulation");
    }
    for w in ebsp.windows(4) {
        assert!(w[..3] != [0, 0, 3] || w[3] <= 3);
    }
    let mut back = Vec::new();
    remove_emulation_prevention(&ebsp, &mut back);
    assert_eq!(back, data);
    // A user data SEI NAL unit reads back exactly.
    let (uuid, payload) = data.split_at(data.len().min(16));
    let mut id = [0u8; 16];
    id[..uuid.len()].copy_from_slice(uuid);
    let payload = &payload[..payload.len().min(2000)];
    for codec in [VideoCodec::H264, VideoCodec::H265] {
        let mut au = Vec::new();
        assert!(user_data_sei(codec, 1 + (data.len() % 7) as u8, &id, payload, &mut au));
        let mut f = Vec::new();
        find_user_data(codec, &au, &mut f);
        assert_eq!(f.len(), 1);
        assert_eq!((f[0].uuid, f[0].payload.as_slice()), (id, payload));
    }
    assert!(!user_data_sei(VideoCodec::Mpeg2, 1, &id, payload, &mut Vec::new()));
}

/// Raw bytes: the first byte picks settings, the rest is the stream.
pub fn demux(data: &[u8]) {
    let (control, ts) = data.split_first().map_or((0, &[][..]), |(c, rest)| (*c, rest));
    let mut config = Config::default();
    config.sync_confirmations = 1 + usize::from(control & 3);
    config.probe_pes = u32::from((control >> 2) & 7);
    let mut subs = Vec::new();
    if control & 0x20 != 0 {
        subs.push(Subscription::new(Select::All, Unit::Pes));
        subs.push(Subscription::new(Select::Psi, Unit::Sections));
    }
    if control & 0x40 != 0 {
        subs.push(Subscription::new(Select::All, Unit::Packets));
    }
    let report = check_stream(ts, &config, &subs, hash(data));
    if config == Config::default() {
        assert_eq!(report, mpegts::analyze(ts), "analyze() differs from Demuxer");
    }
}

/// Structured: the bytes are a program that builds a transport stream out
/// of tables, PES packets, PCRs and corruptions, so mutations explore
/// stream structure instead of mostly failing sync.
pub fn stream(data: &[u8]) {
    let mut input = Input::new(data);
    let config = config_from(&mut input);
    let subs = subscriptions_from(&mut input);
    let ts = generate(&mut input);
    check_stream(&ts, &config, &subs, hash(data));
}

/// The C API: the first part of the input is a sequence of API calls, the
/// rest builds the stream they push. With the `mux` feature the calls
/// also drive the write side.
#[cfg(feature = "capi")]
pub fn capi(data: &[u8]) {
    let mut input = Input::new(data);
    let ops_len = usize::from(input.u8()) * 2;
    let mut ops = Input::new(input.bytes(ops_len));
    let ts = generate(&mut input);
    // SAFETY: every pointer handed to the API points at live, correctly
    // sized memory owned by this function.
    unsafe {
        capi_session(&mut ops, &ts);
        #[cfg(feature = "mux")]
        mux::capi_mux_session(&mut Input::new(data));
        #[cfg(feature = "mux")]
        inject::capi_inject_session(&mut Input::new(data));
    }
}

// ---------------------------------------------------------------- input

/// Reads values from fuzz input; reads past the end return zeros.
pub struct Input<'a> {
    data: &'a [u8],
}

impl<'a> Input<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Input { data }
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    pub fn u8(&mut self) -> u8 {
        match self.data.split_first() {
            Some((b, rest)) => {
                self.data = rest;
                *b
            }
            None => 0,
        }
    }

    pub fn u16(&mut self) -> u16 {
        u16::from_be_bytes([self.u8(), self.u8()])
    }

    pub fn u32(&mut self) -> u32 {
        u32::from_be_bytes([self.u8(), self.u8(), self.u8(), self.u8()])
    }

    pub fn u64(&mut self) -> u64 {
        (u64::from(self.u32()) << 32) | u64::from(self.u32())
    }

    pub fn bool(&mut self) -> bool {
        self.u8() & 1 != 0
    }

    /// Up to `n` bytes.
    pub fn bytes(&mut self, n: usize) -> &'a [u8] {
        let (a, b) = self.data.split_at(n.min(self.data.len()));
        self.data = b;
        a
    }

    /// `n` bytes: as many as the input has, then a repeating pattern.
    pub fn filled(&mut self, n: usize) -> Vec<u8> {
        let mut v = self.bytes(n.min(64)).to_vec();
        let fill = v.last().copied().unwrap_or(0xA5);
        v.resize(n, fill);
        v
    }
}

/// FNV-1a, to derive push sizes from the input deterministically.
fn hash(data: &[u8]) -> u64 {
    data.iter().fold(0xCBF2_9CE4_8422_2325, |h, &b| (h ^ u64::from(b)).wrapping_mul(0x100_0000_01B3)) | 1
}

// ---------------------------------------------------------------- settings

pub fn config_from(input: &mut Input<'_>) -> Config {
    let b = input.u8();
    let mut c = Config::default();
    c.sync_confirmations = 1 + usize::from(b & 3);
    c.probe_pes = u32::from((b >> 2) & 7);
    if b & 0x20 != 0 {
        // Tight limits, so overflow and truncation paths run.
        c.queue_max_items = 1 + usize::from(input.u8() % 16);
        c.queue_max_bytes = 188 + usize::from(input.u16()) * 2;
        c.max_pes_bytes = 188 + usize::from(input.u16() % 4096);
        c.max_reassembly_bytes = c.max_pes_bytes + usize::from(input.u16() % 8192);
        c.max_streams = 1 + usize::from(input.u8() % 8);
    }
    if b & 0x40 != 0 {
        c.overflow = Overflow::DropOldest;
    }
    if b & 0x80 != 0 {
        c.history_packets = usize::from(input.u8()) * 4;
    }
    c.validate().expect("harness built an invalid config");
    c
}

pub fn subscriptions_from(input: &mut Input<'_>) -> Vec<Subscription> {
    (0..input.u8() % 5).map(|_| subscription_from(input.u8(), input.u8())).collect()
}

fn subscription_from(a: u8, b: u8) -> Subscription {
    let select = match a % 6 {
        0 => Select::Pid(PIDS[usize::from(b) % PIDS.len()]),
        1 => Select::Program(u16::from(b % 5)),
        2 => Select::Codec(Codec::ALL[usize::from(b) % Codec::ALL.len()]),
        3 => Select::Kind(StreamKind::ALL[usize::from(b) % StreamKind::ALL.len()]),
        4 => Select::Psi,
        _ => Select::All,
    };
    let unit = [Unit::Packets, Unit::Pes, Unit::Sections][usize::from(a / 6) % 3];
    let mut sub = Subscription::new(select, unit);
    if a & 0x40 != 0 {
        sub = sub.strip_metadata_au();
    }
    if a & 0x80 != 0 {
        sub.with_replay()
    } else {
        sub
    }
}

// ---------------------------------------------------------------- generator

/// PIDs the generator draws from, so tables and streams meet.
const PIDS: [u16; 14] = [
    0x0000, 0x0001, 0x0010, 0x0011, 0x0012, 0x0014, 0x0100, 0x0101, 0x0102, 0x0103, 0x1000, 0x1001, 0x1FFB, 0x1FFF,
];
/// PIDs usually given to PMTs and to elementary streams.
const PMT_PIDS: [u16; 4] = [0x1000, 0x1001, 0x1002, 0x0020];
const ES_PIDS: [u16; 6] = [0x0100, 0x0101, 0x0102, 0x0103, 0x0200, 0x0201];
const STREAM_TYPES: [u8; 18] = [
    0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x0D, 0x0F, 0x11, 0x15, 0x1B, 0x24, 0x33, 0x81, 0x86, 0x87, 0x90, 0xEA,
];
const STREAM_IDS: [u8; 8] = [0xE0, 0xC0, 0xBD, 0xFC, 0xBF, 0xBE, 0xFD, 0xF0];
const TABLE_IDS: [u8; 12] = [0x00, 0x01, 0x02, 0x03, 0x40, 0x42, 0x4E, 0x70, 0x74, 0xC0, 0xFC, 0xFF];
const FOURCCS: [&[u8; 4]; 8] = [b"KLVA", b"AC-3", b"HEVC", b"VANC", b"ID3 ", b"Opus", b"CUEI", b"BSSD"];

struct Gen<'a, 'b> {
    input: &'b mut Input<'a>,
    ts: Ts,
    out: Vec<u8>,
    stride: usize,
    packets: usize,
    /// What earlier tables announced, so later ones mostly agree with them.
    programs: Vec<(u16, u16)>,
    es_pids: Vec<u16>,
    pcr_pids: Vec<u16>,
    last_pat: Option<Vec<u8>>,
}

/// Build a transport stream from fuzz input.
pub fn generate(input: &mut Input<'_>) -> Vec<u8> {
    let stride = match input.u8() % 8 {
        1 => 192,
        2 => 204,
        _ => 188,
    };
    let mut g = Gen {
        input,
        ts: Ts::new(),
        out: Vec::new(),
        stride,
        packets: 0,
        programs: Vec::new(),
        es_pids: Vec::new(),
        pcr_pids: Vec::new(),
        last_pat: None,
    };
    while !g.input.is_empty() && g.packets < MAX_PACKETS {
        match g.input.u8() % 12 {
            0 => g.pat(),
            1 | 2 => g.pmt(),
            3 | 4 => g.pes(),
            5 => g.sections(),
            6 => g.raw_packet(),
            7 => g.pcr(),
            8 => g.cat(),
            9 => g.corrupt(),
            10 => g.ts.null(),
            _ => g.continuation(),
        }
        g.flush();
    }
    g.out
}

impl Gen<'_, '_> {
    /// Move finished packets to the output in the chosen framing.
    fn flush(&mut self) {
        let fresh = mem::take(&mut self.ts.out);
        for p in fresh.chunks(188) {
            match self.stride {
                192 => {
                    self.out.extend_from_slice(&(self.packets as u32).to_be_bytes());
                    self.out.extend_from_slice(p);
                }
                204 => {
                    self.out.extend_from_slice(p);
                    self.out.extend_from_slice(&[0; 16]);
                }
                _ => self.out.extend_from_slice(p),
            }
            self.packets += 1;
        }
    }

    fn pid(&mut self) -> u16 {
        let b = self.input.u8();
        if b < 240 {
            PIDS[usize::from(b) % PIDS.len()]
        } else {
            self.input.u16() & 0x1FFF
        }
    }

    fn pmt_pid(&mut self) -> u16 {
        match self.input.u8() {
            b if b < 220 => PMT_PIDS[usize::from(b) % PMT_PIDS.len()],
            _ => self.pid(),
        }
    }

    fn fourcc(&mut self) -> &'static [u8; 4] {
        FOURCCS[usize::from(self.input.u8()) % FOURCCS.len()]
    }

    fn descriptor(&mut self) -> Vec<u8> {
        match self.input.u8() % 12 {
            0 => registration(b"KLVA"),
            1 => {
                let id = self.fourcc();
                registration(id)
            }
            2 => metadata_descriptor(b"KLVA"),
            3 => {
                let id = self.fourcc();
                metadata_descriptor(id)
            }
            4 => language(&[self.input.u8(), self.input.u8(), self.input.u8()]),
            5 => vec![0x6A, 1, 0],
            6 => vec![0x7A, 1, 0],
            7 => vec![0x59, 8, b'e', b'n', b'g', 0x10, 0, 1, 0, 1],
            8 => vec![0x56, 5, b'e', b'n', b'g', 0x09, 0x00],
            9 => {
                let ext = [0x0E, 0x15, 0x20, 0x21, self.input.u8()][usize::from(self.input.u8() % 5)];
                vec![0x7F, 1, ext]
            }
            10 => vec![0x09, 4, self.input.u8(), self.input.u8(), 0xE0 | (self.input.u8() & 0x1F), self.input.u8()],
            _ => {
                // Raw, possibly with a length that overruns.
                let tag = self.input.u8();
                let len = self.input.u8() % 16;
                let mut d = vec![tag, len];
                let n = usize::from(len.saturating_sub(self.input.u8() % 2));
                d.extend(self.input.filled(n));
                d
            }
        }
    }

    fn descriptors(&mut self) -> Vec<u8> {
        (0..self.input.u8() % 3).flat_map(|_| self.descriptor()).collect()
    }

    /// Send one section (or several, or broken ones) on `pid`.
    fn send_section(&mut self, pid: u16, mut sec: Vec<u8>) {
        match self.input.u8() % 16 {
            0 => *sec.last_mut().unwrap() ^= 1, // bad CRC
            1 if sec.len() > 2 => sec[2] = sec[2].wrapping_add(self.input.u8() % 8), // wrong length
            2 => sec.truncate(sec.len() / 2), // cut short
            _ => {}
        }
        self.ts.section(pid, &sec);
    }

    /// Usually one of `known`, otherwise any PID.
    fn known_pid(&mut self, known: fn(&Self) -> &[u16]) -> u16 {
        let b = self.input.u8();
        let list = known(self);
        if b < 200 && !list.is_empty() {
            list[usize::from(b) % list.len()]
        } else {
            self.pid()
        }
    }

    fn pat(&mut self) {
        // Real feeds repeat the same PAT; changes are the exception.
        if let Some(sec) = self.last_pat.clone() {
            if self.input.u8() < 170 {
                return self.send_section(0, sec);
            }
        }
        let programs: Vec<(u16, u16)> =
            (0..1 + self.input.u8() % 4).map(|_| (u16::from(self.input.u8() % 5), self.pmt_pid())).collect();
        let (tsid, version) = (self.input.u16(), self.input.u8() & 31);
        let sec = pat(tsid, version, &programs);
        let pid = if self.input.u8() < 240 { 0 } else { self.pid() };
        self.programs = programs.into_iter().filter(|p| p.0 != 0).collect();
        self.last_pat = Some(sec.clone());
        self.send_section(pid, sec);
    }

    fn pmt(&mut self) {
        let b = self.input.u8();
        let (program, pid) = if b < 220 && !self.programs.is_empty() {
            self.programs[usize::from(b) % self.programs.len()]
        } else {
            (u16::from(self.input.u8() % 5), self.pid())
        };
        let version = self.input.u8() & 31;
        let prog_desc = self.descriptors();
        let streams: Vec<(u8, u16, Vec<u8>)> = (0..self.input.u8() % 5)
            .map(|_| {
                let b = self.input.u8();
                let st = if b < 230 { STREAM_TYPES[usize::from(b) % STREAM_TYPES.len()] } else { self.input.u8() };
                let pid = match self.input.u8() {
                    b if b < 200 => ES_PIDS[usize::from(b) % ES_PIDS.len()],
                    _ => self.pid(),
                };
                (st, pid, self.descriptors())
            })
            .collect();
        let pcr_pid = match streams.first() {
            Some(s) if self.input.u8() < 200 => s.1,
            _ => self.pid(),
        };
        self.es_pids.extend(streams.iter().map(|s| s.1));
        self.es_pids.dedup();
        self.es_pids.truncate(16);
        if !self.pcr_pids.contains(&pcr_pid) && self.pcr_pids.len() < 8 {
            self.pcr_pids.push(pcr_pid);
        }
        let sec = pmt(program, version, pcr_pid, &prog_desc, &streams);
        self.send_section(pid, sec);
    }

    fn cat(&mut self) {
        let body = self.descriptors();
        let sec = section(0x01, 0xFFFF, self.input.u8() & 31, &body);
        self.send_section(1, sec);
    }

    /// Several sections packed into one payload, with stuffing, a nonzero
    /// pointer_field or a bad CRC.
    fn sections(&mut self) {
        let pid = self.pid();
        let pointer = self.input.u8() % 4;
        let mut payload = vec![pointer];
        payload.extend(std::iter::repeat(0xAB).take(usize::from(pointer)));
        for _ in 0..1 + self.input.u8() % 3 {
            let table_id = TABLE_IDS[usize::from(self.input.u8()) % TABLE_IDS.len()];
            let (ext, version, flags) = (self.input.u16(), self.input.u8() & 31, self.input.u8());
            let n = usize::from(self.input.u16() % 1100);
            let body = self.input.filled(n);
            let mut sec = section_full(table_id, ext, version, flags & 1 != 0, flags >> 4, flags >> 4, &body);
            match flags % 8 {
                1 => *sec.last_mut().unwrap() ^= 0x80,
                2 => sec[1] &= 0x7F, // short form
                3 => sec[1] |= 0x0F, // huge length
                _ => {}
            }
            payload.extend(sec);
        }
        if self.input.bool() {
            payload.extend([0xFF; 20]);
        }
        for (i, c) in payload.chunks(184).enumerate() {
            self.ts.psi_raw(pid, i == 0, c);
        }
    }

    fn opts(&mut self) -> Opts {
        let f = self.input.u8();
        Opts {
            tei: f & 0x1F == 1,
            scrambling: if f & 0x1F == 2 { 2 } else { 0 },
            discontinuity: f & 0x1F == 3,
            cc: (f & 0x1F == 4).then_some(f >> 5),
            ..Opts::default()
        }
    }

    /// Payload split into packets; only the first carries `first`'s flags.
    fn send_payload(&mut self, pid: u16, first: Opts, data: &[u8], psi: bool) {
        let room = if first.discontinuity { 182 } else { 184 };
        let (head, tail) = data.split_at(room.min(data.len()));
        self.ts.packet_with(pid, Opts { pusi: true, ..first }, head, psi);
        let plain = Opts { tei: first.tei, scrambling: first.scrambling, ..Opts::default() };
        for c in tail.chunks(184) {
            self.ts.packet_with(pid, plain, c, psi);
        }
    }

    fn pes(&mut self) {
        let pid = self.known_pid(|g| &g.es_pids);
        let b = self.input.u8();
        let stream_id = if b < 240 { STREAM_IDS[usize::from(b) % STREAM_IDS.len()] } else { self.input.u8() };
        let t = self.input.u8();
        let pts = (t & 1 != 0).then(|| self.input.u64() & ((1 << 33) - 1));
        let dts = (t & 3 == 3).then(|| self.input.u64() & ((1 << 33) - 1));
        let mut es = match self.input.u8() % 6 {
            0 => KLV_KEY.to_vec(),
            // A metadata access unit cell holding a KLV key; the bytes
            // added below make its declared length right only sometimes.
            5 => {
                let mut cell = vec![self.input.u8(), self.input.u8(), 0xDF, 0, self.input.u8() % 64];
                cell.extend_from_slice(&KLV_KEY);
                cell
            }
            1 => vec![0, 0, 0, 1, 0x67],
            2 => vec![0xFF, 0xF1],
            3 => vec![0x0B, 0x77],
            _ => Vec::new(),
        };
        let n = usize::from(self.input.u16() % 1500);
        es.extend(self.input.filled(n));
        let mut bytes = pes_bytes(stream_id, pts, dts, &es);
        match self.input.u8() % 8 {
            0 => bytes[4..6].copy_from_slice(&[0, 0]), // unbounded
            1 => bytes[5] = bytes[5].wrapping_add(self.input.u8()), // wrong length
            2 => {
                let i = usize::from(self.input.u8()) % bytes.len().min(19);
                bytes[i] = self.input.u8(); // damaged header
            }
            _ => {}
        }
        let opts = self.opts();
        self.send_payload(pid, opts, &bytes, false);
    }

    /// Payload without a start indicator, as after a lost PES start.
    fn continuation(&mut self) {
        let pid = self.known_pid(|g| &g.es_pids);
        let n = usize::from(self.input.u8()) % 185;
        let data = self.input.filled(n);
        let opts = self.opts();
        self.ts.packet_with(pid, Opts { pusi: false, ..opts }, &data[..data.len().min(182)], false);
    }

    fn raw_packet(&mut self) {
        let mut p = vec![0x47];
        p.extend(self.input.filled(187));
        self.ts.out.extend(p);
    }

    fn pcr(&mut self) {
        let pid = self.known_pid(|g| &g.pcr_pids);
        let pcr = self.input.u64() % ((1 << 33) * 300);
        let discontinuity = self.input.u8() < 16;
        self.ts.packet_with(pid, Opts { pcr: Some(pcr), discontinuity, no_payload: true, ..Opts::default() }, &[], false);
    }

    /// Damage what has been produced so far.
    fn corrupt(&mut self) {
        let len = self.out.len();
        let stride = self.stride;
        let header = len.checked_sub(stride).map(|start| start + if stride == 192 { 4 } else { 0 });
        match self.input.u8() % 6 {
            0 if len > 0 => {
                let i = self.input.u32() as usize % len;
                self.out[i] ^= 1 << (self.input.u8() % 8);
            }
            1 => {
                let n = usize::from(self.input.u8() % 188).min(len);
                self.out.truncate(len - n);
            }
            2 => {
                let n = usize::from(self.input.u8());
                let junk = self.input.filled(n);
                self.out.extend(junk);
            }
            3 if len >= stride => {
                let last = self.out[len - stride..].to_vec();
                self.out.extend(last); // exact duplicate
            }
            4 if len >= stride => self.out.truncate(len - stride),
            5 => {
                if let Some(h) = header {
                    self.out[h + 1] |= 0x80; // transport_error_indicator
                }
            }
            _ => {}
        }
    }
}

// ---------------------------------------------------------------- checks

/// Run the stream whole, in random chunks and as a live session, checking
/// each run. Returns the report from the whole run.
pub fn check_stream(ts: &[u8], config: &Config, subs: &[Subscription], seed: u64) -> Report {
    let (whole, whole_out) = run_batch(ts, config, subs, None);
    let (split, split_out) = run_batch(ts, config, subs, Some(seed));
    assert_eq!(whole, split, "report depends on how the input was split");
    assert_eq!(whole_out.len(), split_out.len(), "output count depends on how the input was split");
    assert!(whole_out == split_out, "outputs depend on how the input was split");
    check_report(&whole, config, ts.len());
    run_live(ts, config, subs, seed);
    whole
}

/// Push everything (whole, or in random chunks), then collect.
fn run_batch(ts: &[u8], config: &Config, subs: &[Subscription], chunks: Option<u64>) -> (Report, Vec<Output>) {
    let mut d = Demuxer::with_config(config.clone()).unwrap();
    for s in subs {
        d.subscribe(*s).unwrap();
    }
    match chunks {
        None => d.push(ts),
        Some(seed) => {
            let mut rng = Rng(seed);
            let mut rest = ts;
            while !rest.is_empty() {
                let max = if rng.below(4) == 0 { 8 } else { 600 };
                let n = (1 + rng.below(max)).min(rest.len());
                d.push(&rest[..n]);
                rest = &rest[n..];
                check_limits(&d, config);
            }
        }
    }
    d.finish();
    let outputs = drain_all(&mut d, config);
    (d.report(), outputs)
}

/// After `finish`: nothing may still be held for reassembly, and the queue
/// must hold exactly the outputs and bytes it claims.
fn drain_all(d: &mut Demuxer, config: &Config) -> Vec<Output> {
    check_limits(d, config);
    let before = d.output_stats();
    assert_eq!(before.reassembly_bytes, 0, "reassembly bytes still held after finish");
    let outputs: Vec<Output> = d.outputs().collect();
    for o in &outputs {
        check_output(o, config);
    }
    assert_eq!(outputs.len(), before.pending, "queue item count is wrong");
    assert_eq!(outputs.iter().map(|o| o.data.len()).sum::<usize>(), before.pending_bytes, "queue byte count is wrong");
    let after = d.output_stats();
    assert_eq!((after.pending, after.pending_bytes), (0, 0));
    assert_eq!(after.queued - before.queued, 0, "draining queued something");
    outputs
}

/// Push in random chunks while collecting, subscribing, unsubscribing and
/// taking reports, as a live application would.
fn run_live(ts: &[u8], config: &Config, subs: &[Subscription], seed: u64) {
    let mut d = Demuxer::with_config(config.clone()).unwrap();
    let mut ids: Vec<SubscriptionId> = subs.iter().map(|s| d.subscribe(*s).unwrap()).collect();
    let mut rng = Rng(seed.rotate_left(17) | 1);
    let mut revision = d.revision();
    let mut pushed = 0;
    let mut rest = ts;
    while !rest.is_empty() {
        let n = (1 + rng.below(1500)).min(rest.len());
        d.push(&rest[..n]);
        rest = &rest[n..];
        pushed += n;
        assert!(d.revision() >= revision, "revision went backwards");
        revision = d.revision();
        check_limits(&d, config);
        match rng.below(10) {
            0 => {
                let sub = subscription_from(rng.next() as u8, rng.next() as u8);
                match d.subscribe(sub) {
                    Ok(id) => ids.push(id),
                    Err(e) => assert!(ids.len() >= 256, "valid subscription refused: {e}"),
                }
            }
            1 if !ids.is_empty() => {
                let id = ids.swap_remove(rng.below(ids.len()));
                assert!(d.unsubscribe(id));
                assert!(!d.unsubscribe(id), "unsubscribed twice");
            }
            2 => check_report(&d.report(), config, pushed),
            3 => {
                let history: Vec<_> = d.recent_packets().collect();
                assert!(history.len() <= config.history_packets);
                assert!(history.windows(2).all(|w| w[0].0 < w[1].0), "history out of order");
            }
            4 => {
                d.finish(); // end of one input, then more data: allowed
                check_limits(&d, config);
            }
            _ => {}
        }
        for _ in 0..rng.below(64) {
            let Some(o) = d.pop_output() else { break };
            check_output(&o, config);
            if rng.below(2) == 0 {
                d.recycle(o);
            }
        }
    }
    d.finish();
    d.finish();
    drain_all(&mut d, config);
    check_report(&d.report(), config, ts.len());
}

fn check_limits(d: &Demuxer, config: &Config) {
    let s = d.output_stats();
    assert!(s.pending <= config.queue_max_items, "queue over its item limit");
    assert!(s.pending_bytes <= config.queue_max_bytes, "queue over its byte limit");
    assert!(s.reassembly_bytes <= config.max_reassembly_bytes, "reassembly over budget");
}

pub fn check_output(o: &Output, config: &Config) {
    assert!(o.packet > 0);
    assert!(o.pid <= 0x1FFF);
    assert_eq!(o.payload(), &o.data[o.payload_offset.min(o.data.len())..]);
    match o.unit {
        Unit::Packets => {
            assert_eq!(o.data.len(), 188);
            assert_eq!(o.data[0], 0x47, "packet output without sync byte");
            assert_eq!((u16::from(o.data[1] & 0x1F) << 8) | u16::from(o.data[2]), o.pid);
        }
        Unit::Pes => {
            assert!(o.data.len() <= config.max_pes_bytes, "PES over max_pes_bytes");
            assert!(o.payload_offset <= o.data.len());
            assert_eq!(o.table_id, None);
            if !o.damaged {
                assert_eq!(&o.data[..3], &[0, 0, 1], "undamaged PES without start code");
                assert_eq!(o.stream_id, Some(o.data[3]));
            }
            if o.dts.is_some() {
                assert!(o.pts.is_some(), "DTS without PTS");
            }
            for t in [o.pts, o.dts].into_iter().flatten() {
                assert!(t < 1 << 33);
            }
        }
        Unit::Sections => {
            let d = &o.data;
            assert!(d.len() >= 3 && d.len() <= 4096, "section of {} bytes", d.len());
            assert_eq!(d.len(), 3 + ((usize::from(d[1] & 0x0F) << 8) | usize::from(d[2])), "section length");
            assert_eq!(o.table_id, Some(d[0]));
            assert_ne!(d[0], 0xFF, "stuffing delivered as a section");
            if let Some(ok) = o.crc_ok {
                assert_eq!(ok, crc32_mpeg2(d) == 0, "crc_ok wrong");
            }
            assert!(o.pts.is_none() && o.stream_id.is_none() && !o.truncated);
        }
        _ => unreachable!(),
    }
}

pub fn check_report(r: &Report, config: &Config, input_len: usize) {
    let s = &r.stats;
    if let Some(size) = s.packet_size {
        assert!([188, 192, 204].contains(&size));
    }
    assert!(
        s.packets * 188 + s.bytes_skipped <= input_len as u64,
        "{} packets and {} skipped bytes from {input_len} bytes",
        s.packets,
        s.bytes_skipped
    );
    assert!(r.pids.windows(2).all(|w| w[0].pid < w[1].pid), "PIDs not sorted");
    assert_eq!(r.pids.iter().map(|p| p.packets).sum::<u64>(), s.packets, "PID counts do not add up");
    for p in &r.pids {
        assert!(p.pid <= 0x1FFF && p.packets > 0 && p.last_packet <= s.packets);
        assert!(p.scrambled_packets <= p.packets && p.transport_errors <= p.packets);
    }
    assert!(r.programs.windows(2).all(|w| w[0].number < w[1].number), "programs not sorted");
    assert!(r.streams().count() <= config.max_streams, "more streams than max_streams");
    for p in &r.programs {
        assert!(p.pmt_pid <= 0x1FFF);
        assert_eq!(r.program(p.number).map(|q| q.pmt_pid), Some(p.pmt_pid));
        for st in &p.streams {
            assert_eq!(st.program, p.number);
            assert!(st.pid <= 0x1FFF);
            assert!(st.packets <= s.packets && st.scrambled_packets <= st.packets);
            assert_eq!(st.kind(), st.codec.kind());
            assert_eq!(Codec::from_code(st.codec.code()), Some(st.codec));
            for t in [st.first_pts, st.last_pts, st.last_dts].into_iter().flatten() {
                assert!(t < 1 << 33);
            }
            assert!(r.stream(st.pid).is_some());
            let _ = st.duration();
        }
    }
    for k in StreamKind::ALL {
        assert_eq!(r.count(k), r.streams_of(k).count());
    }
    let _ = (r.duration(), r.bitrate(), r.active_streams(10).count());
    black_box(format!("{r}"));
}

// ---------------------------------------------------------------- C API

#[cfg(feature = "capi")]
pub(crate) fn expect(status: mpegts_status, allowed: &[mpegts_status], call: &str) {
    assert!(allowed.contains(&status), "{call} returned {status}");
}

#[cfg(feature = "capi")]
/// Read every byte, so a bad pointer is caught (by a sanitizer, or a crash).
unsafe fn touch(data: *const u8, len: usize) {
    if len > 0 {
        assert!(!data.is_null(), "null data with length {len}");
        black_box(std::slice::from_raw_parts(data, len).iter().fold(0u8, |a, &b| a ^ b));
    }
}

#[cfg(feature = "capi")]
/// A `struct_size` for `T`: usually exact, sometimes an older (shorter) or
/// newer (longer) struct, sometimes invalid.
fn struct_size<T>(ops: &mut Input<'_>) -> u32 {
    let full = mem::size_of::<T>() as u32;
    match ops.u8() % 8 {
        0 => u32::from(ops.u8()) % full,
        1 => full + u32::from(ops.u8()),
        _ => full,
    }
}

#[cfg(feature = "capi")]
const OUTPUT_FIELDS_END: usize = mem::size_of::<mpegts_output>();

#[cfg(feature = "capi")]
unsafe fn capi_session(ops: &mut Input<'_>, ts: &[u8]) {
    const OK: &[i32] = &[MPEGTS_OK];

    let mut config: mpegts_config = mem::zeroed();
    config.struct_size = mem::size_of::<mpegts_config>() as u32;
    expect(mpegts_config_default(&mut config), OK, "config_default");
    match ops.u8() % 4 {
        0 => {}
        1 => {
            config.queue_max_items = 1 + u64::from(ops.u8() % 16);
            config.queue_max_bytes = 188 + u64::from(ops.u16());
            config.history_packets = u64::from(ops.u8());
            config.overflow = u32::from(ops.u8() % 2);
        }
        2 => {
            // Anything, valid or not.
            config.sync_confirmations = u32::from(ops.u8());
            config.probe_pes = u32::from(ops.u8());
            config.overflow = u32::from(ops.u8() % 3);
            config.max_streams = u64::from(ops.u16());
            config.history_packets = u64::from(ops.u16());
            config.queue_max_items = u64::from(ops.u16());
            config.queue_max_bytes = u64::from(ops.u32() % (1 << 24));
            config.max_pes_bytes = u64::from(ops.u32() % (1 << 24));
            config.max_reassembly_bytes = u64::from(ops.u32() % (1 << 25));
        }
        _ => config.struct_size = struct_size::<mpegts_config>(ops),
    }
    let mut d: *mut mpegts_demuxer = ptr::null_mut();
    let config_ptr: *const mpegts_config = if ops.u8() < 32 { ptr::null() } else { &config };
    let status = mpegts_demuxer_new(config_ptr, &mut d);
    expect(status, &[MPEGTS_OK, MPEGTS_ERR_INVALID], "demuxer_new");
    if status != MPEGTS_OK {
        assert!(d.is_null(), "failed demuxer_new left a handle");
        expect(mpegts_demuxer_new(ptr::null(), &mut d), OK, "demuxer_new(defaults)");
    }
    assert!(!d.is_null());

    let mut pos = 0usize;
    let mut ids: Vec<u32> = Vec::new();
    let mut revision = 0u64;
    while !ops.is_empty() {
        match ops.u8() % 11 {
            0 => {
                let n = (1 + usize::from(ops.u16() % 4096)).min(ts.len() - pos);
                expect(mpegts_demuxer_push(d, ts[pos..].as_ptr(), n), OK, "push");
                pos += n;
            }
            1 => {
                expect(mpegts_demuxer_push(d, ptr::null(), 0), OK, "push(NULL, 0)");
                expect(mpegts_demuxer_push(d, ptr::null(), 1), &[MPEGTS_ERR_NULL], "push(NULL, 1)");
            }
            2 => expect(mpegts_demuxer_finish(d), OK, "finish"),
            3 => {
                let mut sub = mpegts_subscription {
                    struct_size: struct_size::<mpegts_subscription>(ops),
                    select: u32::from(ops.u8() % 8),
                    value: match ops.u8() % 4 {
                        0 => ops.u32(),
                        1 => Codec::ALL[usize::from(ops.u8()) % Codec::ALL.len()].code(),
                        _ => u32::from(PIDS[usize::from(ops.u8()) % PIDS.len()]),
                    },
                    unit: u32::from(ops.u8() % 5),
                    replay: u32::from(ops.bool()),
                    strip_metadata_au: u32::from(ops.bool()),
                };
                let mut id = 0;
                let status = mpegts_demuxer_subscribe(d, &sub, &mut id);
                expect(status, &[MPEGTS_OK, MPEGTS_ERR_INVALID], "subscribe");
                if status == MPEGTS_OK {
                    assert!(id != 0 && !ids.contains(&id), "subscription id reused");
                    ids.push(id);
                }
                sub.struct_size = mem::size_of::<mpegts_subscription>() as u32;
                expect(mpegts_demuxer_subscribe(d, &sub, ptr::null_mut()), &[MPEGTS_ERR_NULL], "subscribe(id=NULL)");
            }
            4 => {
                let id = if !ids.is_empty() && ops.bool() {
                    ids.swap_remove(usize::from(ops.u8()) % ids.len())
                } else {
                    ops.u32()
                };
                let status = mpegts_demuxer_unsubscribe(d, id);
                expect(status, &[MPEGTS_OK, MPEGTS_ERR_NOT_FOUND], "unsubscribe");
                expect(mpegts_demuxer_unsubscribe(d, id), &[MPEGTS_ERR_NOT_FOUND], "unsubscribe twice");
            }
            5 => {
                let size = struct_size::<mpegts_output>(ops);
                next_output(d, size);
            }
            6 => {
                let mut stats: mpegts_output_stats = mem::zeroed();
                stats.struct_size = struct_size::<mpegts_output_stats>(ops);
                let status = mpegts_demuxer_output_stats(d, &mut stats);
                expect(status, &[MPEGTS_OK, MPEGTS_ERR_INVALID], "output_stats");
                assert!(stats.struct_size as usize <= mem::size_of::<mpegts_output_stats>() || status != MPEGTS_OK);
            }
            7 => {
                let mut len = 0;
                expect(mpegts_demuxer_history_len(d, &mut len), OK, "history_len");
                let index = if len > 0 && ops.bool() { u64::from(ops.u32()) % len } else { u64::from(ops.u32()) };
                let (mut packet, mut data) = (0u64, ptr::null::<u8>());
                let status = mpegts_demuxer_history_get(d, index, &mut packet, &mut data);
                expect(status, &[MPEGTS_OK, MPEGTS_ERR_RANGE], "history_get");
                assert_eq!(status == MPEGTS_OK, index < len);
                if status == MPEGTS_OK {
                    touch(data, 188);
                    assert_eq!(*data, 0x47);
                }
                expect(mpegts_demuxer_history_get(d, 0, ptr::null_mut(), &mut data), &[MPEGTS_ERR_NULL], "history_get(NULL)");
            }
            8 => {
                let mut r = ptr::null_mut();
                expect(mpegts_demuxer_report(d, &mut r), OK, "report");
                walk_report(r, ops);
                mpegts_report_free(r);
            }
            9 => {
                let mut rev = 0;
                expect(mpegts_demuxer_revision(d, &mut rev), OK, "revision");
                assert!(rev >= revision, "revision went backwards");
                revision = rev;
            }
            _ => {
                let code = ops.u32() % 0x600;
                let name = mpegts_codec_name(code);
                assert_eq!(name.is_null(), Codec::from_code(code).is_none());
                if !name.is_null() {
                    assert!(!CStr::from_ptr(name).to_bytes().is_empty());
                    assert_eq!(mpegts_codec_kind(code), Codec::from_code(code).unwrap().kind().code());
                }
                let kind = u32::from(ops.u8() % 8);
                assert_eq!(mpegts_kind_name(kind).is_null(), StreamKind::from_code(kind).is_none());
                black_box(CStr::from_ptr(mpegts_status_string(ops.u32() as i32)));
            }
        }
    }

    // Finish the stream and drain everything.
    if pos < ts.len() {
        expect(mpegts_demuxer_push(d, ts[pos..].as_ptr(), ts.len() - pos), OK, "push(rest)");
    }
    expect(mpegts_demuxer_finish(d), OK, "finish");
    while next_output(d, mem::size_of::<mpegts_output>() as u32) == MPEGTS_OK {}
    let mut r = ptr::null_mut();
    expect(mpegts_demuxer_report(d, &mut r), OK, "report");
    mpegts_demuxer_free(d);
    // A report is a snapshot that outlives its demuxer.
    walk_report(r, &mut Input::new(&[]));
    mpegts_report_free(r);
}

#[cfg(feature = "capi")]
unsafe fn next_output(d: *mut mpegts_demuxer, size: u32) -> mpegts_status {
    let mut out: mpegts_output = mem::zeroed();
    out.struct_size = size;
    let status = mpegts_demuxer_next_output(d, &mut out);
    expect(status, &[MPEGTS_OK, MPEGTS_EMPTY, MPEGTS_ERR_INVALID], "next_output");
    assert_eq!(status == MPEGTS_ERR_INVALID, size < 4);
    if status == MPEGTS_OK && out.struct_size as usize >= OUTPUT_FIELDS_END {
        assert!((1..=3).contains(&out.unit), "unit {}", out.unit);
        assert!(out.payload_offset <= out.data_len);
        touch(out.data, out.data_len as usize);
        if out.unit == 1 {
            assert_eq!(out.data_len, 188);
        }
    }
    status
}

#[cfg(feature = "capi")]
unsafe fn walk_report(r: *const mpegts_report, ops: &mut Input<'_>) {
    const OK: &[i32] = &[MPEGTS_OK];
    const RANGE: &[i32] = &[MPEGTS_ERR_RANGE];
    let mut summary: mpegts_report_summary = mem::zeroed();
    summary.struct_size = mem::size_of::<mpegts_report_summary>() as u32;
    expect(mpegts_report_summary_get(r, &mut summary), OK, "summary_get");

    let (mut tag, mut data, mut len) = (0u8, ptr::null::<u8>(), 0u64);
    for pi in 0..=summary.program_count {
        let mut p: mpegts_program_info = mem::zeroed();
        p.struct_size = struct_size::<mpegts_program_info>(ops);
        let status = mpegts_report_program(r, pi, &mut p);
        if pi == summary.program_count {
            expect(status, RANGE, "program(past end)");
            continue;
        }
        let full = p.struct_size as usize == mem::size_of::<mpegts_program_info>();
        expect(status, &[MPEGTS_OK, MPEGTS_ERR_INVALID], "program");
        if status != MPEGTS_OK || !full {
            p.struct_size = mem::size_of::<mpegts_program_info>() as u32;
            expect(mpegts_report_program(r, pi, &mut p), OK, "program");
        }
        for di in 0..=p.descriptor_count {
            let status = mpegts_report_program_descriptor(r, pi, di, &mut tag, &mut data, &mut len);
            if di == p.descriptor_count {
                expect(status, RANGE, "program_descriptor(past end)");
            } else {
                expect(status, OK, "program_descriptor");
                touch(data, len as usize);
            }
        }
        for si in 0..=p.stream_count {
            let mut s: mpegts_stream_info = mem::zeroed();
            s.struct_size = mem::size_of::<mpegts_stream_info>() as u32;
            let status = mpegts_report_stream(r, pi, si, &mut s);
            if si == p.stream_count {
                expect(status, RANGE, "stream(past end)");
                continue;
            }
            expect(status, OK, "stream");
            assert!(!mpegts_codec_name(s.codec).is_null(), "unknown codec code {}", s.codec);
            assert_eq!(s.kind, mpegts_codec_kind(s.codec));
            assert_eq!(s.language[3], 0, "language not NUL-terminated");
            assert_eq!(s.program, p.number);
            for di in 0..=s.descriptor_count {
                let status = mpegts_report_stream_descriptor(r, pi, si, di, &mut tag, &mut data, &mut len);
                if di == s.descriptor_count {
                    expect(status, RANGE, "stream_descriptor(past end)");
                } else {
                    expect(status, OK, "stream_descriptor");
                    touch(data, len as usize);
                }
            }
        }
    }
    for i in 0..=summary.pid_count {
        let mut p: mpegts_pid_info = mem::zeroed();
        p.struct_size = mem::size_of::<mpegts_pid_info>() as u32;
        let status = mpegts_report_pid(r, i, &mut p);
        expect(status, if i == summary.pid_count { RANGE } else { OK }, "pid");
    }
    for i in 0..=summary.ca_system_count {
        let (mut system, mut pid) = (0u16, 0u16);
        let status = mpegts_report_ca_system(r, i, &mut system, &mut pid);
        expect(status, if i == summary.ca_system_count { RANGE } else { OK }, "ca_system");
    }
    // Far out of range, and NULL outputs.
    let mut p: mpegts_program_info = mem::zeroed();
    p.struct_size = mem::size_of::<mpegts_program_info>() as u32;
    expect(mpegts_report_program(r, u32::MAX, &mut p), RANGE, "program(u32::MAX)");
    expect(mpegts_report_program(r, 0, ptr::null_mut()), &[MPEGTS_ERR_NULL, MPEGTS_ERR_RANGE], "program(NULL)");
    expect(mpegts_report_summary_get(ptr::null(), ptr::null_mut()), &[MPEGTS_ERR_NULL], "summary_get(NULL)");
}
