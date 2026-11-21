//! Synthetic input streams for the injector tests: programs with video
//! (optionally B-frames), PCR, PSI at a chosen cadence, existing KLV,
//! null padding to a constant packet rate, and the stream events the
//! injector must survive (PTS wrap, time base discontinuity, PMT PID
//! change, scrambling). Every packet's arrival time is known exactly, and
//! every PCR equals the arrival time of its packet.

use super::common::*;

pub const FRAME_TICKS: u64 = 3003;
pub const PTS_WRAP: u64 = 1 << 33;
pub const PCR_WRAP: u64 = PTS_WRAP * 300;

/// A stream the input program already carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Existing {
    /// Synchronous KLV (0x15 + metadata_descriptor) with this service id,
    /// one access unit per frame.
    SyncKlv { pid: u16, service: u8 },
    /// Asynchronous KLV (0x06 + "KLVA" registration), one item per frame.
    AsyncKlv { pid: u16 },
    /// AAC audio, one frame per video frame.
    Audio { pid: u16 },
}

#[derive(Debug, Clone)]
pub struct Prog {
    pub number: u16,
    pub pmt_pid: u16,
    pub video_pid: u16,
    pub pcr_pid: u16,
    pub existing: Vec<Existing>,
}

impl Prog {
    pub fn new(number: u16, pmt_pid: u16, video_pid: u16) -> Self {
        Prog {
            number,
            pmt_pid,
            video_pid,
            pcr_pid: video_pid,
            existing: Vec::new(),
        }
    }

    pub fn with(mut self, e: Existing) -> Self {
        self.existing.push(e);
        self
    }
}

#[derive(Debug, Clone)]
pub struct Synth {
    pub programs: Vec<Prog>,
    pub frames: u64,
    /// Packets per frame interval (a constant packet rate); the rest after
    /// the content is null packets.
    pub packets_per_frame: usize,
    pub video_bytes: usize,
    pub b_frames: bool,
    /// DTS of the first frame.
    pub first_dts: u64,
    /// DTS minus arrival time of each frame's first packet, 90 kHz.
    pub lead: u64,
    /// PAT and PMTs every this many frames.
    pub psi_every: u64,
    /// At this frame the clock and timestamps jump by `jump` (27 MHz,
    /// signed) with a discontinuity_indicator on the PCR.
    pub discontinuity_at: Option<u64>,
    pub jump: i64,
    /// At this frame program 1's PMT moves to this PID (PAT version + 1).
    pub pmt_pid_change: Option<(u64, u16)>,
    /// Scramble the first program's video packets.
    pub scrambled: bool,
    /// PCRs in adaptation-field-only packets instead of video packets.
    pub pcr_separate: bool,
    /// The video access unit of each frame; by default an access unit
    /// delimiter and filler to `video_bytes`.
    pub video: Option<fn(u64) -> Vec<u8>>,
}

impl Default for Synth {
    fn default() -> Self {
        Synth {
            programs: vec![Prog::new(1, 0x1000, 0x100)],
            frames: 90,
            packets_per_frame: 40,
            video_bytes: 3000,
            b_frames: false,
            first_dts: 900_000,
            lead: 36_000,
            psi_every: 3,
            discontinuity_at: None,
            jump: 0,
            pmt_pid_change: None,
            scrambled: false,
            pcr_separate: false,
            video: None,
        }
    }
}

/// What the generator made.
#[derive(Debug, Clone, Default)]
pub struct Made {
    pub ts: Vec<u8>,
    /// Per frame of program 1, in decode order: (PTS, DTS, packet index of
    /// its first packet).
    pub frames: Vec<(u64, u64, usize)>,
    /// Existing KLV of program 1 per frame: (PID, PTS or none, bytes).
    pub klv: Vec<(u16, Option<u64>, Vec<u8>)>,
}

/// Decode-order frame `n` of an I P B B P B B ... stream is shown at this
/// position.
pub fn shown(n: u64) -> u64 {
    if n == 0 {
        0
    } else if n % 3 == 1 {
        n + 2
    } else {
        n - 1
    }
}

/// A PES over packets; only the first carries `first`'s adaptation field.
pub fn send_pes(ts: &mut Ts, pid: u16, first: Opts, pes: &[u8]) {
    let af = match (first.pcr, first.discontinuity) {
        (Some(_), _) => 8,
        (None, true) => 2,
        _ => 0,
    };
    let (head, tail) = pes.split_at((184 - af).min(pes.len()));
    ts.packet_with(
        pid,
        Opts {
            pusi: true,
            ..first
        },
        head,
        false,
    );
    let plain = Opts {
        scrambling: first.scrambling,
        ..Opts::default()
    };
    for c in tail.chunks(184) {
        ts.packet_with(pid, plain, c, false);
    }
}

/// A fake ST 0601 packet: the key, a BER length and filler.
pub fn klv_packet(seed: u64, len: usize) -> Vec<u8> {
    let mut v = KLV_KEY.to_vec();
    v.extend([0x82, (len >> 8) as u8, len as u8]);
    let mut rng = Rng(seed | 1);
    v.extend((0..len).map(|_| rng.next() as u8));
    v
}

impl Synth {
    pub fn make(&self) -> Made {
        let ppf = self.packets_per_frame as u64;
        let mut ts = Ts::new();
        let mut made = Made::default();
        let start_clock = ((self.first_dts + PTS_WRAP - self.lead % PTS_WRAP) % PTS_WRAP) * 300;
        let mut offset: i64 = 0;
        let mut pat_version = 0u8;
        let mut pmt_pids: Vec<u16> = self.programs.iter().map(|p| p.pmt_pid).collect();
        let mut seq = [0u8; 8];
        let wrap = |t: i64, m: u64| -> u64 { t.rem_euclid(m as i64) as u64 };
        for n in 0..self.frames {
            let first_index = ts.out.len() / 188;
            let clock_at = |k: u64, offset: i64| -> u64 {
                wrap(
                    start_clock as i64 + (k * FRAME_TICKS * 300 / ppf) as i64 + offset,
                    PCR_WRAP,
                )
            };
            let mut discontinuity = false;
            if self.discontinuity_at == Some(n) {
                offset += self.jump;
                discontinuity = true;
            }
            if let Some((at, pid)) = self.pmt_pid_change {
                if at == n {
                    pmt_pids[0] = pid;
                    pat_version += 1;
                }
            }
            let ts_offset = offset / 300;
            if n % self.psi_every == 0 || self.pmt_pid_change.is_some_and(|(at, _)| at == n) {
                let entries: Vec<(u16, u16)> = self
                    .programs
                    .iter()
                    .zip(&pmt_pids)
                    .map(|(p, &pid)| (p.number, pid))
                    .collect();
                ts.section(0, &pat(1, pat_version, &entries));
                for (p, &pid) in self.programs.iter().zip(&pmt_pids) {
                    let mut streams = vec![(H264, p.video_pid, vec![])];
                    for e in &p.existing {
                        streams.push(match *e {
                            Existing::SyncKlv { pid, service } => {
                                let mut d = metadata_descriptor(b"KLVA");
                                d[9] = service;
                                d.extend_from_slice(&[
                                    0x27, 0x09, 0xFF, 0xFF, 0xFF, 0xC0, 0x04, 0x00, 0xC0, 0x00,
                                    0x00,
                                ]);
                                (METADATA, pid, d)
                            }
                            Existing::AsyncKlv { pid } => (PRIVATE, pid, registration(b"KLVA")),
                            Existing::Audio { pid } => (AAC, pid, vec![]),
                        });
                    }
                    ts.section(pid, &pmt(p.number, 0, p.pcr_pid, &[], &streams));
                }
            }
            for (pi, p) in self.programs.iter().enumerate() {
                let dts = wrap(
                    self.first_dts as i64 + (n * FRAME_TICKS) as i64 + ts_offset,
                    PTS_WRAP,
                );
                let pts = if self.b_frames {
                    wrap(
                        self.first_dts as i64 + ((shown(n) + 1) * FRAME_TICKS) as i64 + ts_offset,
                        PTS_WRAP,
                    )
                } else {
                    dts
                };
                let es = match self.video {
                    Some(f) => f(n),
                    None => {
                        let mut es = vec![0, 0, 0, 1, 0x09, 0xF0];
                        es.resize(self.video_bytes, 0xA0 ^ n as u8);
                        es
                    }
                };
                let klv_pids: Vec<u16> = p
                    .existing
                    .iter()
                    .filter_map(|e| match *e {
                        Existing::SyncKlv { pid, .. } | Existing::AsyncKlv { pid } => Some(pid),
                        Existing::Audio { .. } => None,
                    })
                    .collect();
                // Where this frame's PCR goes: in the video packet, in the
                // first packet of a KLV PES on the PCR PID, or alone.
                let pcr_in_video = !self.pcr_separate && p.pcr_pid == p.video_pid;
                let pcr_in_klv = !self.pcr_separate && klv_pids.contains(&p.pcr_pid);
                let pcr_now = |ts: &Ts, offset: i64| clock_at((ts.out.len() / 188) as u64, offset);
                if !pcr_in_video && !pcr_in_klv {
                    let o = Opts {
                        pcr: Some(pcr_now(&ts, offset)),
                        discontinuity,
                        no_payload: true,
                        ..Opts::default()
                    };
                    ts.packet_with(p.pcr_pid, o, &[], false);
                }
                let o = Opts {
                    pcr: pcr_in_video.then(|| pcr_now(&ts, offset)),
                    discontinuity: discontinuity && pcr_in_video,
                    scrambling: if self.scrambled && pi == 0 { 2 } else { 0 },
                    ..Opts::default()
                };
                let video_pts_dts = if self.b_frames && pts != dts {
                    (Some(pts), Some(dts))
                } else {
                    (Some(pts), None)
                };
                if pi == 0 {
                    made.frames
                        .push((pts, video_pts_dts.1.unwrap_or(pts), (ts.out.len() / 188)));
                }
                send_pes(
                    &mut ts,
                    p.video_pid,
                    o,
                    &pes_bytes(0xE0, video_pts_dts.0, video_pts_dts.1, &es),
                );
                for (ei, e) in p.existing.iter().enumerate() {
                    let klv_opts = |ts: &Ts, pid: u16| Opts {
                        pcr: (pcr_in_klv && pid == p.pcr_pid).then(|| pcr_now(ts, offset)),
                        discontinuity: discontinuity && pcr_in_klv && pid == p.pcr_pid,
                        ..Opts::default()
                    };
                    match *e {
                        Existing::SyncKlv { pid, service } => {
                            let data = klv_packet(n * 31 + ei as u64, 40);
                            let s = &mut seq[ei];
                            let mut cell =
                                vec![service, *s, 0xDF, (data.len() >> 8) as u8, data.len() as u8];
                            *s = s.wrapping_add(1);
                            cell.extend_from_slice(&data);
                            let mut pes = pes_bytes(0xFC, Some(pts), None, &cell);
                            pes[6] = 0x85;
                            let o = klv_opts(&ts, pid);
                            send_pes(&mut ts, pid, o, &pes);
                            if pi == 0 {
                                made.klv.push((pid, Some(pts), data));
                            }
                        }
                        Existing::AsyncKlv { pid } => {
                            let data = klv_packet(n * 37 + ei as u64, 30);
                            let mut pes = pes_bytes(0xBD, None, None, &data);
                            pes[6] = 0x85;
                            let o = klv_opts(&ts, pid);
                            send_pes(&mut ts, pid, o, &pes);
                            if pi == 0 {
                                made.klv.push((pid, None, data));
                            }
                        }
                        Existing::Audio { pid } => {
                            ts.pes(
                                pid,
                                0xC0,
                                Some(pts),
                                &[0xFF, 0xF1, 0x50, 0x80, 0x02, 0x1F, 0xFC, 0, 0, 0],
                            );
                        }
                    }
                }
            }
            let used = ts.out.len() / 188 - first_index;
            assert!(
                used <= self.packets_per_frame,
                "frame {n} needs {used} packets, more than packets_per_frame"
            );
            for _ in used..self.packets_per_frame {
                ts.null();
            }
        }
        made.ts = ts.out;
        made
    }
}
