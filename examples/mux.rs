//! Write a synthetic transport stream with the `mux` write primitives:
//! H.264 video, synchronous KLV (MISB ST 1402.2) and asynchronous KLV
//! (SMPTE RP 217) in one program.
//!
//!     cargo run --features mux --example mux -- OUT.ts [options]
//!
//! --frames N          frames to write at 30000/1001 fps (default 300)
//! --oversized BYTES   also send one synchronous KLV access unit of this
//!                     many bytes (random content) in frame 10, to show the
//!                     fragmentation of access units over 65,522 bytes
//!
//! Every frame is a 16x16 IDR picture coded as I_PCM, built here bit by
//! bit, so the video decodes without an encoder. Each frame carries an ST
//! 0601 packet (Precision Time Stamp, mission id, version, checksum) in the
//! synchronous KLV stream with the frame's PTS; every third frame the same
//! packet also goes into the asynchronous stream. PAT and PMT go out every
//! frame (at most 100 ms apart, ST 1402.2), a PCR with every frame (at most
//! 40 ms apart). Check the result with any player or with
//!
//!     ffprobe -show_streams -show_packets OUT.ts

use mpegts::mux::{MuxError, PesUnit, PesWriter, ProgramTables, StreamSpec};
use std::{env, fs, process};

/// PIDs of the program.
pub const PMT_PID: u16 = 0x1000;
pub const VIDEO_PID: u16 = 0x100;
pub const SYNC_KLV_PID: u16 = 0x101;
pub const ASYNC_KLV_PID: u16 = 0x102;
/// 90 kHz ticks per frame at 30000/1001 fps.
pub const FRAME_TICKS: u64 = 3003;
/// PTS of the first frame (1 s).
pub const FIRST_PTS: u64 = 90_000;

/// What to write.
pub struct Options {
    pub frames: u64,
    /// (frame, bytes): one oversized synchronous KLV access unit.
    pub oversized: Option<(u64, usize)>,
}

/// The stream as bytes.
pub fn synthetic_stream(options: &Options) -> Result<Vec<u8>, MuxError> {
    let video = StreamSpec::h264();
    let sync = StreamSpec::klv_sync(0);
    let asynchronous = StreamSpec::klv_async();
    let mut tables = ProgramTables::new(1);
    tables.add_program(1, PMT_PID, Some(VIDEO_PID))?;
    tables.add_stream(1, VIDEO_PID, &video)?;
    tables.add_stream(1, SYNC_KLV_PID, &sync)?;
    tables.add_stream(1, ASYNC_KLV_PID, &asynchronous)?;
    let mut video_writer = PesWriter::new(VIDEO_PID, &video)?;
    let mut sync_writer = PesWriter::new(SYNC_KLV_PID, &sync)?;
    let mut async_writer = PesWriter::new(ASYNC_KLV_PID, &asynchronous)?;

    let mut out = Vec::new();
    for frame in 0..options.frames {
        let pts = FIRST_PTS + frame * FRAME_TICKS;
        tables.write_all(&mut out)?;
        // The frame's first packet arrives half a second before its
        // presentation time; the PCR says so.
        let pcr = (pts - 45_000) * 300;
        let picture = h264_frame(frame);
        video_writer.write(&PesUnit::new(&picture).with_pts(pts).with_pcr(pcr).with_random_access(), &mut out)?;
        // Synchronous KLV right after its frame: it arrives within 1 s
        // before its PTS (ST 1402-12) and carries the frame's PTS.
        let klv = st0601_packet(misp_time(frame));
        sync_writer.write(&PesUnit::new(&klv).with_pts(pts), &mut out)?;
        if let Some((at, len)) = options.oversized {
            if at == frame {
                sync_writer.write(&PesUnit::new(&noise(len)).with_pts(pts), &mut out)?;
            }
        }
        if frame % 3 == 0 {
            async_writer.write(&PesUnit::new(&klv), &mut out)?;
        }
    }
    Ok(out)
}

/// MISP time (microseconds) of a frame: an arbitrary start plus the frame
/// period, so ST 0601 tag 2 and the PTS advance together.
pub fn misp_time(frame: u64) -> u64 {
    1_700_000_000_000_000 + frame * 1_000_000 * 1001 / 30_000
}

/// Bit writer for H.264 syntax (ITU-T H.264 7.2: u(n), ue(v), se(v)).
struct Bits {
    out: Vec<u8>,
    cur: u8,
    n: u8,
}

impl Bits {
    fn new() -> Self {
        Bits { out: Vec::new(), cur: 0, n: 0 }
    }

    fn u(&mut self, bits: u32, value: u32) {
        for i in (0..bits).rev() {
            self.cur = self.cur << 1 | ((value >> i) & 1) as u8;
            self.n += 1;
            if self.n == 8 {
                self.out.push(self.cur);
                self.cur = 0;
                self.n = 0;
            }
        }
    }

    fn ue(&mut self, value: u32) {
        let x = value + 1;
        let len = 32 - x.leading_zeros();
        self.u(len - 1, 0);
        self.u(len, x);
    }

    fn se(&mut self, value: i32) {
        self.ue(if value > 0 { 2 * value as u32 - 1 } else { 2 * value.unsigned_abs() });
    }

    fn align_zero(&mut self) {
        while self.n != 0 {
            self.u(1, 0);
        }
    }

    /// rbsp_trailing_bits.
    fn finish(mut self) -> Vec<u8> {
        self.u(1, 1);
        self.align_zero();
        self.out
    }
}

/// A NAL unit in byte stream form: start code, header, payload with
/// emulation prevention (H.264 7.4.1).
fn nal(header: u8, rbsp: &[u8], out: &mut Vec<u8>) {
    out.extend_from_slice(&[0, 0, 0, 1, header]);
    let mut zeros = 0;
    for &b in rbsp {
        if zeros >= 2 && b <= 3 {
            out.push(3);
            zeros = 0;
        }
        out.push(b);
        zeros = if b == 0 { zeros + 1 } else { 0 };
    }
}

/// One access unit: access unit delimiter (required in every AVC access
/// unit in a TS, ISO/IEC 13818-1 2.14.1), SPS, PPS and a 16x16 IDR slice
/// whose single macroblock is I_PCM.
pub fn h264_frame(frame: u64) -> Vec<u8> {
    let mut au = Vec::new();
    // AUD: primary_pic_type 0 (I).
    nal(0x09, &[0x10], &mut au);
    // SPS: constrained baseline, level 1.0, one 16x16 macroblock.
    let mut s = Bits::new();
    s.u(8, 66); // profile_idc
    s.u(8, 0xC0); // constraint_set0/1, reserved zeros
    s.u(8, 10); // level_idc
    s.ue(0); // seq_parameter_set_id
    s.ue(0); // log2_max_frame_num_minus4
    s.ue(2); // pic_order_cnt_type
    s.ue(1); // max_num_ref_frames
    s.u(1, 0); // gaps_in_frame_num_value_allowed_flag
    s.ue(0); // pic_width_in_mbs_minus1
    s.ue(0); // pic_height_in_map_units_minus1
    s.u(1, 1); // frame_mbs_only_flag
    s.u(1, 1); // direct_8x8_inference_flag
    s.u(1, 0); // frame_cropping_flag
    s.u(1, 0); // vui_parameters_present_flag
    nal(0x67, &s.finish(), &mut au);
    // PPS.
    let mut p = Bits::new();
    p.ue(0); // pic_parameter_set_id
    p.ue(0); // seq_parameter_set_id
    p.u(1, 0); // entropy_coding_mode_flag (CAVLC)
    p.u(1, 0); // bottom_field_pic_order_in_frame_present_flag
    p.ue(0); // num_slice_groups_minus1
    p.ue(0); // num_ref_idx_l0_default_active_minus1
    p.ue(0); // num_ref_idx_l1_default_active_minus1
    p.u(1, 0); // weighted_pred_flag
    p.u(2, 0); // weighted_bipred_idc
    p.se(0); // pic_init_qp_minus26
    p.se(0); // pic_init_qs_minus26
    p.se(0); // chroma_qp_index_offset
    p.u(1, 0); // deblocking_filter_control_present_flag
    p.u(1, 0); // constrained_intra_pred_flag
    p.u(1, 0); // redundant_pic_cnt_present_flag
    nal(0x68, &p.finish(), &mut au);
    // IDR slice.
    let mut v = Bits::new();
    v.ue(0); // first_mb_in_slice
    v.ue(7); // slice_type: I, all slices of the picture
    v.ue(0); // pic_parameter_set_id
    v.u(4, 0); // frame_num
    v.ue((frame % 2) as u32); // idr_pic_id, differs between consecutive IDRs
    v.u(1, 0); // no_output_of_prior_pics_flag
    v.u(1, 0); // long_term_reference_flag
    v.se(0); // slice_qp_delta
    v.ue(25); // mb_type I_PCM
    v.align_zero(); // pcm_alignment_zero_bit
    let luma = 16 + (frame * 7 % 220) as u32;
    for _ in 0..256 {
        v.u(8, luma);
    }
    for _ in 0..128 {
        v.u(8, 128);
    }
    nal(0x65, &v.finish(), &mut au);
    au
}

/// A MISB ST 0601 packet: key, BER length, tag 2 (Precision Time Stamp),
/// tag 3 (mission id), tag 65 (version 19) and tag 1 (checksum).
pub fn st0601_packet(misp_time_us: u64) -> Vec<u8> {
    let mut items = vec![0x02, 0x08];
    items.extend_from_slice(&misp_time_us.to_be_bytes());
    let mission = b"MPEGTS-EXAMPLE";
    items.extend([0x03, mission.len() as u8]);
    items.extend_from_slice(mission);
    items.extend([0x41, 0x01, 0x13]);
    let mut packet = vec![0x06, 0x0E, 0x2B, 0x34, 0x02, 0x0B, 0x01, 0x01, 0x0E, 0x01, 0x03, 0x01, 0x01, 0x00, 0x00, 0x00];
    packet.push((items.len() + 4) as u8);
    packet.extend(items);
    packet.extend([0x01, 0x02]);
    // ST 0601 checksum: 16-bit sum over the packet up to here, even bytes
    // in the high byte.
    let sum = packet.iter().enumerate().fold(0u16, |s, (i, &b)| s.wrapping_add(u16::from(b) << (8 * ((i + 1) % 2))));
    packet.extend(sum.to_be_bytes());
    packet
}

/// Deterministic filler (xorshift).
pub fn noise(len: usize) -> Vec<u8> {
    let mut x = 0x2545_F491_4F6C_DD1Du64;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x as u8
        })
        .collect()
}

#[allow(dead_code)]
fn usage(msg: &str) -> ! {
    eprintln!("{msg}\nusage: mux OUT.ts [--frames N] [--oversized BYTES]");
    process::exit(2);
}

#[allow(dead_code)]
fn main() {
    let mut args = env::args().skip(1);
    let mut path = None;
    let mut options = Options { frames: 300, oversized: None };
    while let Some(a) = args.next() {
        let mut number = |name: &str| -> u64 {
            args.next().and_then(|v| v.parse().ok()).unwrap_or_else(|| usage(&format!("{name} needs a number")))
        };
        match a.as_str() {
            "--frames" => options.frames = number("--frames"),
            "--oversized" => options.oversized = Some((10, number("--oversized") as usize)),
            _ if path.is_none() && !a.starts_with("--") => path = Some(a),
            _ => usage(&format!("unknown argument {a}")),
        }
    }
    let path = path.unwrap_or_else(|| usage("missing OUT.ts"));
    let ts = synthetic_stream(&options).unwrap_or_else(|e| {
        eprintln!("{e}");
        process::exit(1);
    });
    if let Err(e) = fs::write(&path, &ts) {
        eprintln!("{path}: {e}");
        process::exit(1);
    }
    eprintln!("wrote {} packets ({} bytes) to {path}", ts.len() / 188, ts.len());
}
