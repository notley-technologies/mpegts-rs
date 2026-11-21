//! User data in the video ([`VideoSei`] scan and write), M2TS records and
//! the extrapolating clock, on synthetic H.264, H.265 and MPEG-2 streams
//! (tests/harness/es.rs), with the patched FFmpeg (`trace_headers`) and
//! TSDuck as oracles when `MPEGTS_FFMPEG` and `MPEGTS_TSDUCK` are set.
mod harness;

use harness::es::{self as es, frame_time, st0604_payload, EsConfig, Video, MISP_H264, MISP_H265, OTHER_UUID};
use harness::synth::*;
use mpegts::mux::*;
use mpegts::nal::{self, UserData, VideoCodec};
use mpegts::{ClockMode, Demuxer, PacketHeader, PesHeader, Select, Subscription, Unit};

const VIDEO_PID: u16 = 0x100;

// ---------------------------------------------------------------- helpers

/// A frame as the callback saw it: its PTS, the user data, what was read of
/// the prefix, and the write's result.
type Seen = (u64, Vec<UserData>, Option<VideoPrefix>, Option<Result<(), InjectError>>);

/// What one run saw and produced.
struct Run {
    out: Vec<u8>,
    stats: InjectStats,
    target: Target,
    events: Vec<InjectEvent>,
    /// Per frame, in decode order.
    frames: Vec<Seen>,
}

type Writer = dyn Fn(u64, &mut Units<'_>) -> Option<Result<(), InjectError>>;

/// Inject, calling `write` for frame `n` (decode order); with `klv`, a sync
/// KLV unit per frame too.
fn try_run(cfg: InjectConfig, input: &[u8], write: &Writer, klv: bool, chunk: usize) -> Result<Run, InjectError> {
    let mut inj = Injector::new(cfg)?;
    let mut out = Vec::new();
    let mut frames = Vec::new();
    let mut reorder = FrameReorder::new(16);
    let mut on_frame = |f: &Frame, u: &mut Units<'_>| {
        let n = frames.len() as u64;
        if klv {
            // Presentation order for the KLV PTS.
            reorder.push(*f);
            while let Some(r) = reorder.pop() {
                u.insert(InsertUnit::sync(r.pts, klv_packet(r.pts, 30))).unwrap();
            }
        } else if u.carriage() == KlvCarriage::Async && n % 3 == 0 {
            u.insert(InsertUnit::asynchronous(klv_packet(n, 30))).unwrap();
        }
        let seen = u.user_data().to_vec();
        let prefix = u.video_prefix();
        let r = write(n, u);
        frames.push((f.pts, seen, prefix, r));
    };
    for c in input.chunks(chunk) {
        inj.push_with(c, &mut out, &mut on_frame)?;
    }
    let stats = inj.finish_with(&mut out, &mut on_frame)?;
    let target = inj.target().cloned().unwrap();
    let events = std::iter::from_fn(|| inj.pop_event()).collect();
    Ok(Run {
        out,
        stats,
        target,
        events,
        frames,
    })
}

fn run(cfg: InjectConfig, input: &[u8], write: &Writer, klv: bool) -> Run {
    try_run(cfg, input, write, klv, 1316).unwrap()
}

fn nothing(_: u64, _: &mut Units<'_>) -> Option<Result<(), InjectError>> {
    None
}

/// The ST 0604-like payload each frame gets written.
fn new_payload(n: u64) -> Vec<u8> {
    st0604_payload(0x9F, frame_time(n) + 1_000_000)
}

/// Write the codec's ST 0604 message, replacing an existing one.
fn write_misp(video: Video) -> impl Fn(u64, &mut Units<'_>) -> Option<Result<(), InjectError>> {
    move |n, u| {
        let uuid = video.misp_uuid();
        Some(u.write_user_data(uuid, &new_payload(n), &[MISP_H264, MISP_H265]))
    }
}

fn config(mode: VideoSei) -> InjectConfig {
    InjectConfig::default().with_video_sei(mode)
}

fn cbr(on_overflow: OnOverflow) -> RateMode {
    RateMode::Cbr { on_overflow }
}

/// A video PES as the demuxer reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct VideoPes {
    header: Vec<u8>,
    es: Vec<u8>,
    pts: Option<u64>,
    dts: Option<u64>,
}

fn video_pes(ts: &[u8]) -> Vec<VideoPes> {
    let mut config = mpegts::Config::default();
    config.max_pes_bytes = 1 << 20;
    let mut d = Demuxer::with_config(config).unwrap();
    d.subscribe(Subscription::new(Select::Pid(VIDEO_PID), Unit::Pes)).unwrap();
    d.push(ts);
    d.finish();
    let report = d.report();
    assert!(report.pids.iter().all(|p| p.cc_errors == 0), "CC errors: {:?}", report.pids);
    d.outputs()
        .map(|o| {
            assert!(!o.damaged && !o.truncated);
            let h = PesHeader::parse(&o.data).unwrap();
            VideoPes {
                header: o.data[..h.payload_offset].to_vec(),
                es: o.data[h.payload_offset..].to_vec(),
                pts: h.pts,
                dts: h.dts,
            }
        })
        .collect()
}

fn pid_of(p: &[u8]) -> u16 {
    u16::from_be_bytes([p[1] & 0x1F, p[2]])
}

/// Every packet of a PID not in `skip` goes out unchanged and in order.
fn assert_invariant(input: &[u8], out: &[u8], skip: &[u16]) {
    let keep = |p: &&[u8]| !skip.contains(&pid_of(p));
    let a: Vec<&[u8]> = input.chunks(188).filter(keep).collect();
    let b: Vec<&[u8]> = out.chunks(188).filter(keep).collect();
    assert_eq!(a.len(), b.len(), "pass-through packet count");
    for (i, (x, y)) in a.iter().zip(&b).enumerate() {
        assert_eq!(x, y, "pass-through packet {i} differs");
    }
}

fn skip(r: &Run) -> Vec<u16> {
    vec![0, r.target.pmt_pid, r.target.klv_pid, VIDEO_PID, 0x1FFF]
}

/// The SEI NAL unit the injector should insert, built independently with
/// the harness's own emulation prevention.
fn sei_nal(video: Video, tid1: u8, uuid: &[u8; 16], payload: &[u8]) -> Vec<u8> {
    let mut rbsp = vec![5];
    let mut size = 16 + payload.len();
    while size >= 255 {
        rbsp.push(0xFF);
        size -= 255;
    }
    rbsp.push(size as u8);
    rbsp.extend_from_slice(uuid);
    rbsp.extend_from_slice(payload);
    rbsp.push(0x80);
    let header = match video {
        Video::H265 => vec![39 << 1, tid1],
        _ => vec![0x06],
    };
    let mut out = Vec::new();
    es::nal(&header, &rbsp, &mut out);
    out
}

fn codec(video: Video) -> VideoCodec {
    match video {
        Video::H264 => VideoCodec::H264,
        Video::H265 => VideoCodec::H265,
        Video::Mpeg2 => VideoCodec::Mpeg2,
    }
}

/// Where a new SEI NAL unit goes in an access unit, and the first VCL NAL
/// unit's nuh_temporal_id_plus1: before the first VCL NAL unit, or before a
/// type-14 prefix NAL unit right before it (R-ST1402TIME-82, -85, -86).
fn insertion_point(video: Video, au: &[u8]) -> (usize, u8) {
    let c = codec(video);
    let mut prev: Option<nal::NalUnit<'_>> = None;
    for u in nal::nal_units(au) {
        let t = u.nal_type(c).unwrap();
        if c.is_vcl(t) {
            let tid1 = if video == Video::H265 { u.data[1] & 7 } else { 1 };
            return match prev {
                Some(p) if video == Video::H264 && p.nal_type(c) == Some(14) => (p.start, tid1),
                _ => (u.start, tid1),
            };
        }
        prev = Some(u);
    }
    panic!("no VCL NAL unit");
}

/// Input and output video PES side by side: the output ES must be the input
/// ES with `expected(n, input es)`, PTS and DTS unchanged, the PES header
/// unchanged except PES_packet_length.
fn check_video(input: &[u8], out: &[u8], expected: &dyn Fn(usize, &[u8]) -> Vec<u8>) {
    let a = video_pes(input);
    let b = video_pes(out);
    assert_eq!(a.len(), b.len(), "video PES count");
    for (n, (x, y)) in a.iter().zip(&b).enumerate() {
        assert_eq!((x.pts, x.dts), (y.pts, y.dts), "PES {n}: PTS/DTS (R-ST1402TIME-09)");
        assert_eq!(x.header[..4], y.header[..4]);
        assert_eq!(x.header[6..], y.header[6..], "PES {n}: header bytes");
        let want = expected(n, &x.es);
        assert_eq!(y.es, want, "PES {n}: elementary stream");
        let old = u16::from_be_bytes([x.header[4], x.header[5]]);
        let new = u16::from_be_bytes([y.header[4], y.header[5]]);
        if old == 0 {
            assert_eq!(new, 0, "PES {n}: unbounded stays unbounded (R-ST1402TIME-14)");
        } else {
            assert_eq!(usize::from(new), usize::from(old) + want.len() - x.es.len(), "PES {n}: PES_packet_length");
        }
    }
}

/// The input ES with the frame's ST 0604 message inserted.
fn inserted(video: Video) -> impl Fn(usize, &[u8]) -> Vec<u8> {
    move |n, au| {
        let (at, tid1) = insertion_point(video, au);
        let mut v = au[..at].to_vec();
        v.extend(sei_nal(video, tid1, &video.misp_uuid(), &new_payload(n as u64)));
        v.extend_from_slice(&au[at..]);
        v
    }
}

fn expected_seen(cfg: &EsConfig, n: u64) -> Vec<([u8; 16], Vec<u8>)> {
    es::expected_user_data(cfg, n)
}

fn seen(r: &Run, n: usize) -> Vec<([u8; 16], Vec<u8>)> {
    r.frames[n].1.iter().map(|m| (m.uuid, m.payload.clone())).collect()
}

fn stream(cfg: EsConfig, b_frames: bool) -> Synth {
    Synth {
        es: Some(cfg),
        b_frames,
        frames: 30,
        ..Synth::default()
    }
}

fn es_with(video: Video, other: bool, misp: bool) -> EsConfig {
    let mut c = EsConfig::new(video);
    c.other = other;
    c.misp = misp;
    c
}

// ---------------------------------------------------------------- scan

#[test]
fn scan_reports_each_access_units_user_data() {
    for video in [Video::H264, Video::H265, Video::Mpeg2] {
        for (other, misp, second) in [(true, true, false), (true, true, true), (false, true, false), (false, false, false)] {
            let mut cfg = es_with(video, other, misp);
            cfg.misp_second = second;
            let made = stream(cfg, true).make();
            let r = run(config(VideoSei::Scan), &made.ts, &nothing, true);
            assert_eq!(r.frames.len(), 30);
            for n in 0..30 {
                assert_eq!(seen(&r, n), expected_seen(&cfg, n as u64), "{video:?} frame {n}");
                let prefix = r.frames[n].2.unwrap();
                assert_eq!(prefix.codec, codec(video));
                assert!(prefix.complete, "{video:?} frame {n}");
            }
            assert_eq!(r.stats.frames_scanned, 30);
            assert_eq!(r.stats.user_data_found as usize, 30 * expected_seen(&cfg, 0).len());
            // Scanning changes nothing.
            let off = run(InjectConfig::default(), &made.ts, &nothing, true);
            assert!(r.out == off.out, "{video:?}: Scan output differs from Off");
            assert_eq!(r.target.video_codec, Some(codec(video)));
            assert_eq!(r.target.video_stream_type, Some(video.stream_type()));
        }
    }
}

#[test]
fn positions_point_at_the_messages() {
    let cfg = es_with(Video::H264, true, true);
    let made = stream(cfg, false).make();
    let r = run(config(VideoSei::Scan), &made.ts, &nothing, false);
    let pes = video_pes(&made.ts);
    for (n, p) in pes.iter().enumerate() {
        assert_eq!(r.frames[n].1.len(), 2);
        for m in &r.frames[n].1 {
            let pos = m.position;
            assert_eq!(p.es[pos.start..pos.header], [0, 0, 0, 1]);
            assert_eq!((p.es[pos.header], pos.nal_type), (0x06, 6));
            let mut again = Vec::new();
            nal::find_user_data(VideoCodec::H264, &p.es[pos.start..pos.end], &mut again);
            assert!(again.iter().any(|a| a.uuid == m.uuid && a.payload == m.payload));
        }
    }
}

#[test]
fn reading_never_changes_the_output() {
    // Off never looks at the video; Scan and Write without writes read it
    // and must give the same bytes, whatever else is configured.
    let made = stream(es_with(Video::H264, true, true), true).make();
    let merged = Synth {
        programs: vec![Prog::new(1, 0x1000, VIDEO_PID).with(Existing::SyncKlv { pid: 0x101, service: 0 })],
        ..stream(es_with(Video::H265, true, false), true)
    }
    .make();
    for (input, base) in [
        (&made.ts, InjectConfig::default()),
        (&made.ts, InjectConfig::default().with_rate(cbr(OnOverflow::Fail))),
        (&made.ts, InjectConfig::default().with_carriage(KlvCarriage::Async)),
        (&merged.ts, InjectConfig::default()),
        (&merged.ts, InjectConfig::default().with_rate(cbr(OnOverflow::DropOldest))),
    ] {
        let klv = base.carriage == KlvCarriage::Sync;
        let off = run(base.clone(), input, &nothing, klv);
        assert_eq!(off.stats.frames_scanned, 0);
        assert!(off.frames.iter().all(|f| f.1.is_empty() && f.2.is_none()));
        for mode in [VideoSei::Scan, VideoSei::Write] {
            let r = run(base.clone().with_video_sei(mode), input, &nothing, klv);
            assert_eq!(r.stats.frames_scanned, 30);
            assert!(r.out == off.out, "{mode:?} with {base:?}");
        }
    }
}

// ---------------------------------------------------------------- insertion

#[test]
fn insert_before_the_first_slice_in_h264() {
    for b_frames in [false, true] {
        let cfg = es_with(Video::H264, true, false);
        let made = stream(cfg, b_frames).make();
        let r = run(config(VideoSei::Write), &made.ts, &write_misp(Video::H264), true);
        assert!(r.frames.iter().all(|f| matches!(f.3, Some(Ok(())))));
        assert_eq!(r.stats.user_data_inserted, 30);
        assert_eq!((r.stats.user_data_rewritten, r.stats.user_data_skipped), (0, 0));
        check_video(&made.ts, &r.out, &inserted(Video::H264));
        assert_invariant(&made.ts, &r.out, &skip(&r));
        // The other message is still first, ours second (after all
        // existing SEI, R-ST1402TIME-82).
        let back = run(config(VideoSei::Scan), &r.out, &nothing, false);
        for n in 0..30 {
            let got = seen(&back, n);
            assert_eq!(got.len(), 2);
            assert_eq!(got[0].0, OTHER_UUID);
            assert_eq!(got[1], (MISP_H264, new_payload(n as u64)));
        }
    }
}

#[test]
fn insert_in_h265_copies_the_temporal_id() {
    let cfg = es_with(Video::H265, true, false);
    let made = stream(cfg, true).make();
    let r = run(config(VideoSei::Write), &made.ts, &write_misp(Video::H265), true);
    assert_eq!(r.stats.user_data_inserted, 30);
    check_video(&made.ts, &r.out, &inserted(Video::H265));
    assert_invariant(&made.ts, &r.out, &skip(&r));
    // B-frames have TemporalId 1: the inserted SEI says so (R-ST1402TIME-85).
    let out = video_pes(&r.out);
    let mut tids = Vec::new();
    for p in &out {
        let mut last_sei = None;
        for u in nal::nal_units(&p.es) {
            match u.nal_type(VideoCodec::H265).unwrap() {
                39 => last_sei = Some(u.data[1]),
                t if t < 32 => {
                    assert_eq!(last_sei, Some(u.data[1]), "SEI right before the slice with its TemporalId");
                    tids.push(u.data[1]);
                    break;
                }
                _ => {}
            }
        }
    }
    assert!(tids.contains(&1) && tids.contains(&2));
}

#[test]
fn insert_before_a_prefix_nal_unit() {
    let mut cfg = es_with(Video::H264, false, false);
    cfg.prefix_nal = true;
    let made = stream(cfg, true).make();
    let r = run(config(VideoSei::Write), &made.ts, &write_misp(Video::H264), false);
    check_video(&made.ts, &r.out, &|n, au| {
        let (at, _) = insertion_point(Video::H264, au);
        // The point is the prefix NAL unit, not the slice.
        assert_eq!(au[at..at + 5], [0, 0, 0, 1, au[at + 4]]);
        assert_eq!(au[at + 4] & 0x1F, 14);
        let mut v = au[..at].to_vec();
        v.extend(sei_nal(Video::H264, 1, &MISP_H264, &new_payload(n as u64)));
        v.extend_from_slice(&au[at..]);
        v
    });
}

#[test]
fn emulation_prevention_protects_the_payload() {
    let cfg = es_with(Video::H264, false, false);
    let made = stream(cfg, false).make();
    fn payload(n: u64) -> Vec<u8> {
        // Status 00 and a zero time, then start code look-alikes
        // (R-ST1402TIME-81).
        let mut p = st0604_payload(0, 0);
        p.extend_from_slice(&[0, 0, 0, 0, 0, 3, 0, 0, 1, 0, 0, 2, n as u8, 0, 0]);
        p
    }
    let write = |n: u64, u: &mut Units<'_>| Some(u.write_user_data(MISP_H264, &payload(n), &[]));
    let r = run(config(VideoSei::Write), &made.ts, &write, false);
    check_video(&made.ts, &r.out, &|n, au| {
        let (at, _) = insertion_point(Video::H264, au);
        let mut v = au[..at].to_vec();
        let sei = sei_nal(Video::H264, 1, &MISP_H264, &payload(n as u64));
        assert!(sei.windows(4).any(|w| w == [0, 0, 3, 0]));
        v.extend(sei);
        v.extend_from_slice(&au[at..]);
        v
    });
    let back = run(config(VideoSei::Scan), &r.out, &nothing, false);
    for n in 0..30 {
        assert_eq!(seen(&back, n), [(MISP_H264, payload(n as u64))]);
    }
}

// ---------------------------------------------------------------- rewrite

#[test]
fn rewrite_in_place() {
    for video in [Video::H264, Video::H265] {
        for second in [false, true] {
            let mut cfg = es_with(video, true, true);
            cfg.misp_second = second;
            let made = stream(cfg, true).make();
            let r = run(config(VideoSei::Write), &made.ts, &write_misp(video), true);
            assert_eq!((r.stats.user_data_rewritten, r.stats.user_data_inserted), (30, 0));
            // Same sizes: the video packets only change in those bytes.
            assert_eq!(r.stats.video_packets_added, 0);
            let back = run(config(VideoSei::Scan), &r.out, &nothing, false);
            for n in 0..30 {
                let got = seen(&back, n);
                let mut want = expected_seen(&cfg, n as u64);
                want[1].1 = new_payload(n as u64);
                assert_eq!(got, want, "{video:?} second={second} frame {n}");
                // Same NAL unit, same place.
                let before: Vec<_> = r.frames[n].1.iter().map(|m| m.position).collect();
                let after: Vec<_> = back.frames[n].1.iter().map(|m| m.position).collect();
                assert_eq!(before, after);
            }
            assert_invariant(&made.ts, &r.out, &skip(&r));
        }
    }
}

#[test]
fn rewrite_that_changes_size() {
    let cfg = es_with(Video::H264, true, true);
    let made = stream(cfg, false).make();
    for len in [4usize, 40, 300] {
        let write = move |n: u64, u: &mut Units<'_>| Some(u.write_user_data(MISP_H264, &vec![0x5A ^ n as u8; len], &[]));
        let r = run(config(VideoSei::Write), &made.ts, &write, false);
        assert_eq!(r.stats.user_data_rewritten, 30);
        let back = run(config(VideoSei::Scan), &r.out, &nothing, false);
        for n in 0..30u64 {
            assert_eq!(seen(&back, n as usize)[1], (MISP_H264, vec![0x5A ^ n as u8; len]));
        }
        check_video(&made.ts, &r.out, &|n, au| {
            // The message's own NAL unit rebuilt; a shorter one is followed
            // by zero bytes so the PES keeps its size.
            let m = &r.frames[n].1[1];
            let mut v = au[..m.position.header].to_vec();
            let nal = sei_nal(Video::H264, 1, &MISP_H264, &vec![0x5A ^ n as u8; len]);
            v.extend_from_slice(&nal[4..]);
            let old = m.position.end - m.position.header;
            v.resize(v.len() + old.saturating_sub(nal.len() - 4), 0);
            v.extend_from_slice(&au[m.position.end..]);
            v
        });
    }
}

// ---------------------------------------------------------------- packets

/// A PES whose last packet keeps `tail` payload bytes (so 182 - tail of
/// stuffing), with the PCR in the first packet: 176 + 184 k + tail.
fn sized(cfg: EsConfig, tail: usize) -> Synth {
    Synth {
        pes_len: Some(176 + 184 * 3 + tail),
        ..stream(cfg, true)
    }
}

#[test]
fn vbr_adds_packets_only_when_stuffing_cannot_take_the_bytes() {
    let cfg = es_with(Video::H264, false, false);
    // 40 bytes in the last packet: 142 bytes of stuffing take the 36.
    let roomy = sized(cfg, 40).make();
    let r = run(config(VideoSei::Write), &roomy.ts, &write_misp(Video::H264), false);
    assert_eq!(r.stats.video_packets_added, 0);
    assert_eq!(r.out.len(), run(config(VideoSei::Scan), &roomy.ts, &nothing, false).out.len());
    check_video(&roomy.ts, &r.out, &inserted(Video::H264));
    // A full last packet: one packet more per access unit.
    let tight = sized(cfg, 184).make();
    let r = run(config(VideoSei::Write), &tight.ts, &write_misp(Video::H264), false);
    assert_eq!(r.stats.video_packets_added, 30);
    let scan = run(config(VideoSei::Scan), &tight.ts, &nothing, false);
    assert_eq!(r.out.len(), scan.out.len() + 30 * 188);
    check_video(&tight.ts, &r.out, &inserted(Video::H264));
    assert_invariant(&tight.ts, &r.out, &skip(&r));
    // The PCR stays on its packet: every PCR of the input is in the output
    // on a video packet, in order.
    let pcrs = |ts: &[u8]| -> Vec<u64> {
        ts.chunks(188)
            .filter_map(|p| PacketHeader::parse(p.try_into().unwrap()).ok().and_then(|h| h.pcr()))
            .collect()
    };
    assert_eq!(pcrs(&tight.ts), pcrs(&r.out));
}

#[test]
fn unbounded_pes_stays_unbounded() {
    let cfg = es_with(Video::H265, true, false);
    let made = Synth {
        video_unbounded: true,
        ..sized(cfg, 184)
    }
    .make();
    let r = run(config(VideoSei::Write), &made.ts, &write_misp(Video::H265), true);
    assert_eq!(r.stats.user_data_inserted, 30);
    check_video(&made.ts, &r.out, &inserted(Video::H265));
}

#[test]
fn pes_length_overflow_leaves_the_access_unit_alone() {
    let cfg = es_with(Video::H264, false, false);
    // PES_packet_length 65520 + 36 would not fit in 16 bits.
    let made = Synth {
        pes_len: Some(6 + 65_520),
        frames: 4,
        packets_per_frame: 400,
        ..stream(cfg, false)
    }
    .make();
    let r = run(config(VideoSei::Write), &made.ts, &write_misp(Video::H264), false);
    assert_eq!((r.stats.user_data_inserted, r.stats.user_data_skipped), (0, 4));
    assert!(r.events.iter().any(|e| matches!(
        e,
        InjectEvent::Warning(InjectWarning::UserDataSkipped { reason: UserDataSkip::PesTooLong, .. })
    )));
    check_video(&made.ts, &r.out, &|_, au| au.to_vec());
}

#[test]
fn cbr_uses_null_packets_and_keeps_the_packet_count() {
    let cfg = es_with(Video::H264, true, false);
    let made = sized(cfg, 184).make();
    let c = config(VideoSei::Write).with_rate(cbr(OnOverflow::Fail));
    let r = run(c, &made.ts, &write_misp(Video::H264), true);
    assert_eq!(r.stats.packets_out, r.stats.packets_in);
    assert_eq!(r.out.len(), made.ts.len());
    assert_eq!(r.stats.video_slots_filled, 30);
    assert_eq!(r.stats.video_packets_added, 0);
    check_video(&made.ts, &r.out, &inserted(Video::H264));
    // No packet moved: every non-null input packet outside the injector's
    // PIDs and the video is at its index.
    for (i, (a, b)) in made.ts.chunks(188).zip(r.out.chunks(188)).enumerate() {
        if !skip(&r).contains(&pid_of(a)) {
            assert_eq!(a, b, "packet {i}");
        }
        let pcr = |p: &[u8]| PacketHeader::parse(p.try_into().unwrap()).unwrap().pcr();
        assert_eq!(pcr(a), pcr(b), "packet {i}: a PCR moved or changed");
    }
}

#[test]
fn cbr_without_room_fails_or_skips() {
    let cfg = es_with(Video::H264, false, false);
    // No null packets and no stuffing: no room anywhere. Five video
    // packets and a PAT and PMT per frame.
    let tight = Synth {
        packets_per_frame: 7,
        psi_every: 1,
        ..sized(cfg, 184)
    };
    let made = tight.make();
    assert!(made.ts.chunks(188).all(|p| pid_of(p) != 0x1FFF));
    let fail = config(VideoSei::Write).with_rate(cbr(OnOverflow::Fail));
    match try_run(fail, &made.ts, &write_misp(Video::H264), false, 1316) {
        Err(InjectError::CbrOverflow { pts: Some(_), .. }) => {}
        other => panic!("expected CbrOverflow, got {:?}", other.map(|r| r.stats)),
    }
    let drop = config(VideoSei::Write).with_rate(cbr(OnOverflow::DropOldest));
    let r = run(drop, &made.ts, &write_misp(Video::H264), false);
    assert_eq!((r.stats.user_data_skipped, r.stats.user_data_inserted), (30, 0));
    assert_eq!(r.out.len(), made.ts.len());
    check_video(&made.ts, &r.out, &|_, au| au.to_vec());
    let skipped = r
        .events
        .iter()
        .filter(|e| {
            matches!(e, InjectEvent::Warning(InjectWarning::UserDataSkipped { reason: UserDataSkip::NoRoom, .. }))
        })
        .count();
    assert_eq!(skipped, 30);
}

// ---------------------------------------------------------------- refusals

#[test]
fn writes_that_are_refused() {
    let cfg = es_with(Video::H264, false, false);
    let made = stream(cfg, false).make();
    // Scan mode does not write.
    let r = run(config(VideoSei::Scan), &made.ts, &write_misp(Video::H264), false);
    assert!(r.frames.iter().all(|f| matches!(f.3, Some(Err(InjectError::UserData(_))))));
    // One write per access unit; a payload over the limit.
    let twice = |n: u64, u: &mut Units<'_>| {
        assert!(matches!(
            u.write_user_data(MISP_H264, &vec![0; MAX_USER_DATA_PAYLOAD + 1], &[]),
            Err(InjectError::UserData(_))
        ));
        u.write_user_data(MISP_H264, &new_payload(n), &[]).unwrap();
        Some(u.write_user_data(MISP_H264, &new_payload(n), &[]))
    };
    let r = run(config(VideoSei::Write), &made.ts, &twice, false);
    assert!(r.frames.iter().all(|f| matches!(f.3, Some(Err(InjectError::UserData(_))))));
    assert_eq!(r.stats.user_data_inserted, 30);
    // MPEG-2 video is read, not written: refused when the target is chosen.
    let m2 = stream(es_with(Video::Mpeg2, true, false), false).make();
    match try_run(config(VideoSei::Write), &m2.ts, &nothing, false, 1316) {
        Err(InjectError::Unsupported(_)) => {}
        other => panic!("{:?}", other.map(|r| r.stats)),
    }
    // A prefix cut by the byte limit (the "other" SEI NAL unit makes it
    // longer than 16 bytes): the callback still runs, the write is refused
    // and counted.
    let long = stream(es_with(Video::H264, true, false), false).make();
    let r = run(
        config(VideoSei::Write).with_video_sei_max_prefix(16),
        &long.ts,
        &write_misp(Video::H264),
        false,
    );
    assert!(r.frames.iter().all(|f| !f.2.unwrap().complete && matches!(f.3, Some(Err(_)))));
    assert_eq!((r.stats.user_data_skipped, r.stats.user_data_inserted), (30, 0));
    assert!(r.frames.iter().all(|f| f.1.is_empty()), "the cut NAL unit is not reported");
    assert!(r.out == run(InjectConfig::default(), &long.ts, &nothing, false).out);
    // An access unit without picture data in its PES: read to the end,
    // not complete.
    let plain = Synth::default().make();
    let r = run(config(VideoSei::Write), &plain.ts, &write_misp(Video::H264), false);
    assert!(r.frames.iter().all(|f| f.2.is_some_and(|p| !p.complete)));
    assert_eq!(r.stats.user_data_skipped, 90);
    assert!(matches!(
        Injector::new(config(VideoSei::Scan).with_video_sei_max_prefix(8)),
        Err(InjectError::InvalidConfig(_))
    ));
}

// ---------------------------------------------------------------- M2TS

const ATS_MASK: u32 = (1 << 30) - 1;

/// The input as M2TS records: arrival_time_stamp 7,000 ticks apart from
/// just below the 30-bit wrap, copy_permission_indicator 1 for the first
/// half and 2 after.
fn to_m2ts(ts: &[u8]) -> Vec<u8> {
    let n = ts.len() / 188;
    let mut out = Vec::new();
    for (i, p) in ts.chunks(188).enumerate() {
        let ats = (ATS_MASK - 500_000).wrapping_add(i as u32 * 7_000) & ATS_MASK;
        let cpi: u32 = if i < n / 2 { 1 } else { 2 };
        out.extend_from_slice(&((cpi << 30) | ats).to_be_bytes());
        out.extend_from_slice(p);
    }
    out
}

fn header(r: &[u8]) -> u32 {
    u32::from_be_bytes([r[0], r[1], r[2], r[3]])
}

fn strip(m2ts: &[u8]) -> Vec<u8> {
    m2ts.chunks(192).flat_map(|r| r[4..].to_vec()).collect()
}

/// Output records against input records. Each input record goes out with
/// its own header (its packet unchanged unless the injector owns its PID
/// or rewrote video in it), or is consumed (`consumed` PIDs); every other
/// record is added: its stamp lies between those of the input records
/// around it, in order, with the copy_permission_indicator of the input
/// record that follows. Returns the records added.
fn check_records(input: &[u8], out: &[u8], consumed: &[u16], changed: &[u16]) -> usize {
    let ins: Vec<&[u8]> = input.chunks(192).collect();
    let stamp = |h: u32| h & ATS_MASK;
    let mut at = 0;
    let mut prev: Option<u32> = None;
    let mut last_off = 0;
    let mut added = 0;
    for r in out.chunks(192) {
        let h = header(r);
        // The slot of one of the next input records, skipping consumed ones.
        let mut j = at;
        let mut slot = None;
        while j < ins.len() {
            if header(ins[j]) == h {
                slot = Some(j);
                break;
            }
            if !consumed.contains(&pid_of(&ins[j][4..])) {
                break;
            }
            j += 1;
        }
        match slot {
            Some(j) => {
                let pid = pid_of(&ins[j][4..]);
                if !consumed.contains(&pid) && !changed.contains(&pid) {
                    assert_eq!(r[4..], ins[j][4..], "record {j} passes through");
                }
                at = j + 1;
                prev = Some(stamp(header(ins[j])));
                last_off = 0;
            }
            None => {
                added += 1;
                let next = header(ins[at]);
                let a = prev.unwrap_or(stamp(next));
                let span = stamp(next).wrapping_sub(a) & ATS_MASK;
                let off = stamp(h).wrapping_sub(a) & ATS_MASK;
                assert!(off >= last_off && off <= span, "added stamp {off} outside {last_off}..={span}");
                last_off = off;
                let ahead: Vec<u32> = ins[at..].iter().take(8).map(|r| header(r) >> 30).collect();
                if ahead.iter().all(|&c| c == ahead[0]) {
                    assert_eq!(h >> 30, ahead[0], "copy_permission_indicator of the next input record");
                }
            }
        }
    }
    added
}

#[test]
fn m2ts_records_in_and_out() {
    let cfg = es_with(Video::H264, true, false);
    let made = sized(cfg, 184).make();
    let m2ts = to_m2ts(&made.ts);
    for rate in [RateMode::Vbr, cbr(OnOverflow::Fail)] {
        let ts_cfg = config(VideoSei::Write).with_rate(rate);
        let ts_run = run(ts_cfg.clone(), &made.ts, &write_misp(Video::H264), true);
        let m2_cfg = ts_cfg.with_record_format(RecordFormat::M2ts);
        // Any chunking, including records cut in the middle.
        let r = try_run(m2_cfg.clone(), &m2ts, &write_misp(Video::H264), true, 1000).unwrap();
        assert_eq!(r.out.len() % 192, 0);
        // The same packets as with 188-byte input.
        assert!(strip(&r.out) == ts_run.out, "{rate:?}: packets differ from the TS run");
        if rate == RateMode::Vbr {
            let added = check_records(&m2ts, &r.out, &[0, r.target.pmt_pid], &[VIDEO_PID]);
            assert!(added > 30 + 30, "PSI, KLV and video packets added");
            assert_eq!(r.stats.video_packets_added, 30);
        } else {
            // CBR: every record is an input record with its header.
            assert_eq!(r.out.len(), m2ts.len());
            for (a, b) in m2ts.chunks(192).zip(r.out.chunks(192)) {
                assert_eq!(header(a), header(b));
            }
        }
        // Records pushed one by one with their clock give the same output
        // as packets pushed with the same clock.
        let mut timeline = mpegts::PcrTimeline::new(None, 1_000_000);
        let mut timed = Vec::new();
        for p in made.ts.chunks(188) {
            timeline.push(p.try_into().unwrap(), &mut |t| timed.push(t));
        }
        timeline.flush(&mut |t| timed.push(t));
        let mut by_packet = Injector::new(config(VideoSei::Write).with_rate(rate)).unwrap();
        let mut by_record = Injector::new(m2_cfg.clone()).unwrap();
        let (mut a, mut b) = (Vec::new(), Vec::new());
        let mut w = |_: &Frame, u: &mut Units<'_>| {
            let _ = u.write_user_data(MISP_H264, &[1, 2, 3], &[]);
        };
        for (i, t) in timed.iter().enumerate() {
            by_packet.push_packet_with(&t.packet, t.clock, &mut a, &mut w).unwrap();
            let rec: &Record = m2ts[i * 192..(i + 1) * 192].try_into().unwrap();
            by_record.push_record_with(rec, t.clock, &mut b, &mut w).unwrap();
        }
        by_packet.finish_with(&mut a, &mut w).unwrap();
        by_record.finish_with(&mut b, &mut w).unwrap();
        assert!(strip(&b) == a);
        // The other input style is refused without ending the injector.
        assert!(matches!(by_record.push_packet(&NULL_PACKET, None, &mut b), Err(InjectError::Usage(_))));
    }
    // A record sink over a closure gets whole records.
    let mut inj = Injector::new(InjectConfig::default().with_record_format(RecordFormat::M2ts)).unwrap();
    let mut n = 0usize;
    let mut sink = RecordFn(|r: &Record| {
        assert_eq!(r[4], 0x47);
        n += 1;
    });
    inj.push(&m2ts, &mut sink).unwrap();
    inj.finish(&mut sink).unwrap();
    assert!(n >= m2ts.len() / 192);
}

// ---------------------------------------------------------------- clock

/// Packets of the PIDs the injector passes through untouched.
fn passed(ts: &[u8]) -> usize {
    ts.chunks(188).filter(|p| ![0, 0x1000, 0x101].contains(&pid_of(p))).count()
}

#[test]
fn extrapolated_clock_holds_nothing() {
    let made = stream(es_with(Video::H264, true, false), false).make();
    for (clock, mode) in [(ClockMode::Extrapolate, VideoSei::Off), (ClockMode::Extrapolate, VideoSei::Scan), (ClockMode::Interpolate, VideoSei::Off)] {
        let cfg = config(mode).with_clock(clock);
        let mut inj = Injector::new(cfg).unwrap();
        let mut out = Vec::new();
        let mut on_frame = |f: &Frame, u: &mut Units<'_>| {
            u.insert(InsertUnit::sync(f.pts, klv_packet(f.pts, 20))).unwrap();
        };
        let mut held_at_some_point = false;
        let mut pushed = 0;
        // Pieces that cut packets, like datagrams of a live feed.
        for chunk in made.ts.chunks(1000) {
            inj.push_with(chunk, &mut out, &mut on_frame).unwrap();
            pushed += chunk.len();
            if inj.target().is_none() {
                continue;
            }
            let complete = pushed / 188 * 188;
            let held = inj.stats().held_packets;
            if clock == ClockMode::Extrapolate {
                // Every whole packet pushed is out (R-ST1402TIME-10).
                assert_eq!(held, 0);
                assert_eq!(passed(&out), passed(&made.ts[..complete]));
            }
            held_at_some_point |= held > 0;
        }
        assert_eq!(held_at_some_point, clock == ClockMode::Interpolate);
        let stats = inj.finish_with(&mut out, &mut on_frame).unwrap();
        assert_eq!(stats.units_inserted, 30);
        assert_eq!(passed(&out), passed(&made.ts));
    }
}

// ---------------------------------------------------------------- oracles

/// One packet (access unit) as `trace_headers` prints it: its PTS and its
/// NAL units, each a title and its fields.
#[derive(Debug, Default)]
struct Traced {
    nals: Vec<(String, Vec<(String, i64)>)>,
}

impl Traced {
    /// The user data messages and, per message, the NAL unit's
    /// nuh_temporal_id_plus1 and its index among the NAL units.
    fn user_data(&self) -> Vec<([u8; 16], Vec<u8>, i64, usize)> {
        let mut out = Vec::new();
        for (i, (title, fields)) in self.nals.iter().enumerate() {
            if !title.contains("Supplemental Enhancement Information") {
                continue;
            }
            let tid = fields.iter().find(|f| f.0 == "nuh_temporal_id_plus1").map_or(0, |f| f.1);
            let mut current: Option<([u8; 16], Vec<u8>)> = None;
            for (name, value) in fields {
                if name == "last_payload_type_byte" {
                    out.extend(current.take().map(|(u, p)| (u, p, tid, i)));
                    current = Some(([0; 16], Vec::new()));
                }
                if let Some(c) = current.as_mut() {
                    if let Some(k) = name.strip_prefix("uuid_iso_iec_11578[") {
                        let k: usize = k.trim_end_matches(']').parse().unwrap();
                        c.0[k] = *value as u8;
                    } else if name.starts_with("user_data_payload_byte[") {
                        c.1.push(*value as u8);
                    }
                }
            }
            out.extend(current.take().map(|(u, p)| (u, p, tid, i)));
        }
        out
    }

    /// Index of the first slice, and its nuh_temporal_id_plus1.
    fn first_slice(&self) -> (usize, i64) {
        let i = self.nals.iter().position(|(t, _)| t.starts_with("Slice")).expect("a slice");
        let tid = self.nals[i].1.iter().find(|f| f.0 == "nuh_temporal_id_plus1").map_or(0, |f| f.1);
        (i, tid)
    }
}

/// `ffmpeg -i ts -map 0:v -c copy -bsf:v trace_headers -f null -`, parsed.
fn trace_headers(ffmpeg: &str, path: &std::path::Path) -> Vec<Traced> {
    let out = std::process::Command::new(ffmpeg)
        .args(["-hide_banner", "-loglevel", "info", "-i"])
        .arg(path)
        .args(["-map", "0:v", "-c", "copy", "-bsf:v", "trace_headers", "-f", "null", "-"])
        .output()
        .unwrap_or_else(|e| panic!("cannot run {ffmpeg}: {e}"));
    let text = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "ffmpeg failed: {text}");
    let mut packets: Vec<Traced> = Vec::new();
    // A title starts a NAL unit when its first field is the header's
    // forbidden_zero_bit; other titles (an SEI payload's) are sections of
    // the current NAL unit.
    let mut title: Option<String> = None;
    for line in text.lines() {
        let Some(at) = line.find("[trace_headers @") else {
            continue;
        };
        let rest = &line[at..];
        let Some(close) = rest.find("] ") else {
            continue;
        };
        let content = rest[close + 2..].trim_end();
        if content.starts_with("Packet:") {
            packets.push(Traced::default());
            title = None;
            continue;
        }
        let Some(p) = packets.last_mut() else {
            continue;
        };
        if content.starts_with(|c: char| c.is_ascii_digit()) {
            let tokens: Vec<&str> = content.split_whitespace().collect();
            let value: i64 = tokens.last().unwrap().parse().unwrap_or(-1);
            if let Some(t) = title.take() {
                if tokens[1] == "forbidden_zero_bit" {
                    p.nals.push((t, Vec::new()));
                }
            }
            if let Some(n) = p.nals.last_mut() {
                n.1.push((tokens[1].to_string(), value));
            }
        } else {
            title = Some(content.to_string());
        }
    }
    packets
}

fn oracle_dir(tool: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("mpegts-video-sei-{tool}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A stream written by the injector, with what each access unit's user data
/// must be: (name, video, output, expected per frame, CBR).
type OracleStream = (&'static str, Video, Run, Vec<Vec<([u8; 16], Vec<u8>)>>, bool);

fn oracle_streams() -> Vec<OracleStream> {
    let mut out = Vec::new();
    for (name, video, misp, rate) in [
        ("h264-insert", Video::H264, false, RateMode::Vbr),
        ("h264-rewrite", Video::H264, true, RateMode::Vbr),
        ("h265-insert", Video::H265, false, RateMode::Vbr),
        ("h265-rewrite", Video::H265, true, RateMode::Vbr),
        ("h264-cbr", Video::H264, false, cbr(OnOverflow::Fail)),
        ("h265-cbr", Video::H265, false, cbr(OnOverflow::Fail)),
    ] {
        let cfg = es_with(video, true, misp);
        // At about 11 Mbit/s, as tests/inject.rs does for pcrverify.
        let made = Synth {
            frames: ORACLE_FRAMES,
            packets_per_frame: 240,
            ..sized(cfg, 184)
        }
        .make();
        let r = run(config(VideoSei::Write).with_rate(rate), &made.ts, &write_misp(video), true);
        let expected = (0..ORACLE_FRAMES)
            .map(|n| {
                let mut v = expected_seen(&cfg, n);
                if misp {
                    v[1].1 = new_payload(n);
                } else {
                    v.push((video.misp_uuid(), new_payload(n)));
                }
                v
            })
            .collect();
        out.push((name, video, r, expected, rate != RateMode::Vbr));
    }
    // Emulation prevention: a zero status and time stamp.
    let cfg = es_with(Video::H265, false, false);
    let made = Synth {
        frames: ORACLE_FRAMES,
        ..sized(cfg, 184)
    }
    .make();
    let zero = |_: u64, u: &mut Units<'_>| Some(u.write_user_data(MISP_H265, &st0604_payload(0, 0), &[]));
    let r = run(config(VideoSei::Write), &made.ts, &zero, false);
    let expected = vec![vec![(MISP_H265, st0604_payload(0, 0))]; ORACLE_FRAMES as usize];
    out.push(("h265-zero", Video::H265, r, expected, false));
    out
}

/// Frames of the oracle streams: enough PCRs for pcrverify to measure the
/// rate.
const ORACLE_FRAMES: u64 = 120;

/// With `MPEGTS_FFMPEG` naming the patched ffmpeg: `trace_headers` parses
/// every access unit of the written streams and shows the written
/// user_data_unregistered message with its identifier and payload, in an
/// SEI NAL unit right before the first slice (insertion) and with the
/// slice's TemporalId (H.265). Skipped when unset.
#[test]
fn ffmpeg_trace_headers_reads_written_sei() {
    let Ok(ffmpeg) = std::env::var("MPEGTS_FFMPEG") else {
        eprintln!("skipping: set MPEGTS_FFMPEG to a patched ffmpeg");
        return;
    };
    let dir = oracle_dir("ffmpeg");
    for (name, video, r, expected, _) in oracle_streams() {
        let path = dir.join(format!("{name}.ts"));
        std::fs::write(&path, &r.out).unwrap();
        let traced = trace_headers(&ffmpeg, &path);
        assert_eq!(traced.len(), ORACLE_FRAMES as usize, "{name}: access units");
        for (n, t) in traced.iter().enumerate() {
            let got = t.user_data();
            let messages: Vec<([u8; 16], Vec<u8>)> = got.iter().map(|m| (m.0, m.1.clone())).collect();
            assert_eq!(messages, expected[n], "{name}: access unit {n}");
            let (slice, tid) = t.first_slice();
            let ours = got.iter().find(|m| m.0 == video.misp_uuid()).unwrap();
            if video == Video::H265 {
                assert_eq!(ours.2, tid, "{name}: access unit {n}: TemporalId of the SEI");
            }
            if !name.ends_with("rewrite") {
                assert_eq!(ours.3 + 1, slice, "{name}: access unit {n}: SEI right before the slice");
            }
        }
        eprintln!("{name}: {} access units traced", traced.len());
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// With `MPEGTS_TSDUCK` naming the directory of TSDuck's `tsp`: no
/// continuity error in any written stream, and `pcrverify` finds no PCR
/// over its jitter limit in the CBR ones. Skipped when unset.
#[test]
fn tsduck_checks_written_video() {
    let Ok(bin) = std::env::var("MPEGTS_TSDUCK") else {
        eprintln!("skipping: set MPEGTS_TSDUCK to the directory of tsp");
        return;
    };
    let tsp = std::path::PathBuf::from(bin).join("tsp");
    let dir = oracle_dir("tsduck");
    for (name, _, r, _, cbr) in oracle_streams() {
        let path = dir.join(format!("{name}.ts"));
        std::fs::write(&path, &r.out).unwrap();
        let mut args = vec!["-P", "continuity"];
        if cbr {
            args.extend(["-P", "pcrverify"]);
        }
        let out = std::process::Command::new(&tsp)
            .arg("-I")
            .arg("file")
            .arg(&path)
            .args(&args)
            .args(["-O", "drop"])
            .output()
            .unwrap_or_else(|e| panic!("cannot run {}: {e}", tsp.display()));
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "{name}: tsp failed: {stderr}");
        let lines: Vec<&str> = stderr.lines().filter(|l| !l.trim().is_empty()).collect();
        if cbr {
            assert_eq!(lines.len(), 1, "{name}: tsp reported: {stderr}");
            assert!(lines[0].contains("PCR OK") && lines[0].contains(" 0 with jitter"), "{name}: {stderr}");
            assert!(!lines[0].contains(" 0 PCR OK"), "{name}: no PCR checked: {stderr}");
            eprintln!("{name}: {}", lines[0]);
        } else {
            assert!(lines.is_empty(), "{name}: tsp reported: {stderr}");
            eprintln!("{name}: continuity clean");
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}
