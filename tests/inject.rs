//! The KLV injector on synthetic streams: read-back, pass-through
//! invariance, the conformance checker in tests/harness/check.rs, and the
//! stream events the injector must handle.
mod harness;

use harness::check::{check, Checked, Expect};
use harness::common::{metadata_descriptor, pat, pmt, Ts, H264, KLV_KEY, METADATA, PRIVATE};
use harness::synth::*;
use mpegts::mux::*;
use mpegts::{Codec, Demuxer, PacketHeader, PcrTimeline, Select, Subscription, Unit};
use std::collections::HashMap;

// ---------------------------------------------------------------- helpers

/// What the frame callback inserts.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Feed {
    Nothing,
    /// A sync unit per frame with the frame's PTS (no B-frames).
    PerFrame,
    /// A sync unit per frame in presentation order through FrameReorder.
    Reordered,
    /// An async unit every third frame.
    Async,
    /// A three-packet sync unit per frame.
    Big,
}

struct Run {
    out: Vec<u8>,
    stats: InjectStats,
    target: Target,
    events: Vec<InjectEvent>,
    frames: Vec<Frame>,
    /// Units inserted, in insertion order.
    inserted: Vec<(Option<u64>, Vec<u8>)>,
}

fn unit_for(pts: u64) -> Vec<u8> {
    klv_packet(pts ^ 0x5A5A, 50 + (pts % 7) as usize)
}

fn try_inject(
    cfg: InjectConfig,
    input: &[u8],
    feed: Feed,
    chunk: usize,
) -> Result<Run, InjectError> {
    let mut inj = Injector::new(cfg)?;
    let mut out = Vec::new();
    let mut frames = Vec::new();
    let mut inserted = Vec::new();
    let mut reorder = FrameReorder::new(16);
    let mut n = 0u64;
    let mut on_frame = |f: &Frame, u: &mut Units<'_>| {
        frames.push(*f);
        n += 1;
        let mut add = |u: &mut Units<'_>, pts: Option<u64>| {
            let data = unit_for(pts.unwrap_or(n));
            let unit = match pts {
                Some(p) => InsertUnit::sync(p, data.clone()),
                None => InsertUnit::asynchronous(data.clone()),
            };
            u.insert(unit).unwrap();
            inserted.push((pts, data));
        };
        match feed {
            Feed::Nothing => {}
            Feed::PerFrame => add(u, Some(f.pts)),
            Feed::Reordered => {
                reorder.push(*f);
                while let Some(r) = reorder.pop() {
                    add(u, Some(r.pts));
                }
            }
            Feed::Async if n % 3 == 0 => add(u, None),
            Feed::Async => {}
            Feed::Big => {
                let data = klv_packet(f.pts, 400);
                u.insert(InsertUnit::sync(f.pts, data.clone())).unwrap();
                inserted.push((Some(f.pts), data));
            }
        }
    };
    for c in input.chunks(chunk) {
        inj.push_with(c, &mut out, &mut on_frame)?;
    }
    let stats = inj.finish_with(&mut out, &mut on_frame)?;
    let target = inj.target().cloned().expect("a target");
    let events = std::iter::from_fn(|| inj.pop_event()).collect();
    Ok(Run {
        out,
        stats,
        target,
        events,
        frames,
        inserted,
    })
}

fn inject(cfg: InjectConfig, input: &[u8], feed: Feed) -> Run {
    try_inject(cfg, input, feed, 1316).unwrap()
}

fn expect(r: &Run, cfg: &InjectConfig) -> Expect {
    Expect {
        program: r.target.program,
        pmt_pid: r.target.pmt_pid,
        pcr_pid: r.target.pcr_pid,
        klv_pid: r.target.klv_pid,
        sync: r.target.carriage == KlvCarriage::Sync,
        service_id: r.target.service_id,
        psi_interval_ms: u64::from(cfg.psi_interval_ms),
        min_lead_ms: u64::from(cfg.min_lead_ms),
        max_lead_ms: u64::from(cfg.max_lead_ms),
        merged: r.target.merged,
    }
}

fn checked(r: &Run, cfg: &InjectConfig) -> Checked {
    let c = check(&r.out, &expect(r, cfg));
    assert!(c.errors.is_empty(), "conformance: {:#?}", c.errors);
    c
}

/// The KLV the demuxer reads back from `pid`, cell headers stripped,
/// joined per PTS (fragments of one access unit).
fn read_back(ts: &[u8], pid: u16) -> Vec<(Option<u64>, Vec<u8>)> {
    let mut config = mpegts::Config::default();
    config.max_pes_bytes = 4 << 20;
    let mut d = Demuxer::with_config(config).unwrap();
    d.subscribe(Subscription::new(Select::Pid(pid), Unit::Pes).strip_metadata_au())
        .unwrap();
    d.push(ts);
    d.finish();
    let r = d.report();
    assert_eq!(r.stats.crc_errors, 0);
    assert!(
        r.pids.iter().all(|p| p.cc_errors == 0),
        "CC errors: {:?}",
        r.pids
    );
    let mut out: Vec<(Option<u64>, Vec<u8>)> = Vec::new();
    let outputs: Vec<mpegts::Output> = d.outputs().collect();
    let n = outputs.len();
    for (i, o) in outputs.into_iter().enumerate() {
        // Only the stream's end may cut a PES short (CBR has no slot left).
        if (o.truncated || o.damaged) && i + 1 == n {
            break;
        }
        assert!(
            !o.damaged && !o.truncated,
            "output {i} of {n}: {:?}",
            (o.damaged, o.truncated, o.pts, o.data.len())
        );
        let continues = o.data.len() > 8 && o.data[6] & 0x04 == 0 && o.pts.is_some();
        match out.last_mut() {
            Some((pts, data)) if continues && *pts == o.pts => data.extend_from_slice(o.payload()),
            _ => out.push((o.pts, o.payload().to_vec())),
        }
    }
    out
}

/// PTS of the PES starting in a packet.
fn pes_pts(p: &[u8]) -> u64 {
    let p: &[u8; 188] = p.try_into().unwrap();
    let h = PacketHeader::parse(p).unwrap();
    mpegts::PesHeader::parse(h.payload(p)).unwrap().pts.unwrap()
}

fn pid_of(p: &[u8]) -> u16 {
    u16::from_be_bytes([p[1] & 0x1F, p[2]])
}

/// Pass-through invariance (MUXING_PLAN.md 7.2): without PID 0, the target
/// PMT PID, the KLV PID and the removed PIDs, output and input are the
/// same packets byte for byte, in the same order (null packets excluded
/// in CBR, where they are the free slots).
fn assert_invariant(input: &[u8], out: &[u8], skip: &[u16], cbr: bool) {
    let keep = |p: &&[u8]| {
        let pid = pid_of(p);
        let free_slot = cbr && pid == 0x1FFF;
        !skip.contains(&pid) && !free_slot
    };
    let a: Vec<&[u8]> = input.chunks(188).filter(keep).collect();
    let b: Vec<&[u8]> = out.chunks(188).filter(keep).collect();
    assert_eq!(a.len(), b.len(), "pass-through packet count");
    for (i, (x, y)) in a.iter().zip(&b).enumerate() {
        assert_eq!(x, y, "pass-through packet {i} differs");
    }
}

fn skip_pids(r: &Run) -> Vec<u16> {
    let mut v = vec![0, r.target.pmt_pid, r.target.klv_pid];
    v.extend(&r.target.stripped);
    v
}

/// Sections of the target PMT in a stream (current ones), in order.
fn pmt_sections(ts: &[u8], pmt_pid: u16) -> Vec<Vec<u8>> {
    let mut d = Demuxer::new();
    d.subscribe(Subscription::new(Select::Pid(pmt_pid), Unit::Sections))
        .unwrap();
    d.push(ts);
    d.finish();
    d.outputs().map(|o| o.data).collect()
}

// ---------------------------------------------------------------- basics

#[test]
fn sync_units_read_back_with_their_pts() {
    let made = Synth::default().make();
    let cfg = InjectConfig::default();
    let r = inject(cfg.clone(), &made.ts, Feed::PerFrame);
    assert_eq!(
        (
            r.target.program,
            r.target.pmt_pid,
            r.target.video_pid,
            r.target.pcr_pid
        ),
        (1, 0x1000, Some(0x100), Some(0x100))
    );
    // D-ISO13818-11: the first free PID above the program's highest
    // elementary PID; D-ISO13818-03: service 0 when free.
    assert_eq!(
        (r.target.klv_pid, r.target.service_id, r.target.merged),
        (0x101, Some(0), false)
    );
    let c = checked(&r, &cfg);
    assert_eq!(c.units.len(), 90);
    assert_eq!(read_back(&r.out, 0x101), r.inserted);
    assert_eq!(
        (r.stats.units_inserted, r.stats.dropped_late, r.stats.frames),
        (90, 0, 90)
    );
    // Each unit right before its frame: the next packet after a KLV PES is
    // the video PES with the same PTS (st1402-time.md D-7).
    let packets: Vec<&[u8]> = r.out.chunks(188).collect();
    let mut checked_frames = 0;
    for (i, p) in packets.iter().enumerate() {
        if pid_of(p) == 0x101 && p[1] & 0x40 != 0 {
            let pts = pes_pts(p);
            let next = packets[i + 1..]
                .iter()
                .find(|q| pid_of(q) != 0x101)
                .unwrap();
            let h = PacketHeader::parse((*next).try_into().unwrap()).unwrap();
            assert_eq!(h.pid, 0x100);
            assert_eq!(
                mpegts::PesHeader::parse(h.payload((*next).try_into().unwrap()))
                    .unwrap()
                    .pts,
                Some(pts)
            );
            checked_frames += 1;
        }
    }
    assert_eq!(checked_frames, 90);
    assert_invariant(&made.ts, &r.out, &skip_pids(&r), false);
    // The PAT went out byte for byte; the PMT gained our entry and one
    // version (D-ISO13818-12).
    let pats_in = pmt_sections(&made.ts, 0);
    let pats_out = pmt_sections(&r.out, 0);
    assert!(pats_out.iter().all(|s| s == &pats_in[0]));
    let pmt_out = &pmt_sections(&r.out, 0x1000)[0];
    let pmt_in = &pmt_sections(&made.ts, 0x1000)[0];
    assert_eq!((pmt_out[5] >> 1) & 0x1F, ((pmt_in[5] >> 1) & 0x1F) + 1);
    let report = mpegts::analyze(&r.out);
    let s = report.stream(0x101).unwrap();
    assert_eq!(
        (s.codec, s.stream_type, s.pes_stream_id),
        (Codec::KlvSync, 0x15, Some(0xFC))
    );
    assert_eq!(
        s.descriptors
            .iter()
            .map(|d| (d.tag, d.data.clone()))
            .collect::<Vec<_>>(),
        [
            (
                0x26,
                vec![0x01, 0x00, 0xFF, b'K', b'L', b'V', b'A', 0x00, 0x0F]
            ),
            (
                0x27,
                vec![0xFF, 0xFF, 0xFF, 0xC0, 0x04, 0x00, 0xC0, 0x00, 0x00]
            )
        ]
    );
    assert!(matches!(r.events.first(), Some(InjectEvent::TargetFound(t)) if t.klv_pid == 0x101));
}

#[test]
fn async_units_read_back_in_order() {
    let made = Synth::default().make();
    let cfg = InjectConfig::default().with_carriage(KlvCarriage::Async);
    let r = inject(cfg.clone(), &made.ts, Feed::Async);
    assert_eq!((r.target.klv_pid, r.target.service_id), (0x101, None));
    let c = checked(&r, &cfg);
    assert_eq!(c.units.len(), 30);
    assert_eq!(read_back(&r.out, 0x101), r.inserted);
    let report = mpegts::analyze(&r.out);
    let s = report.stream(0x101).unwrap();
    assert_eq!(
        (s.codec, s.stream_type, s.registration.unwrap()),
        (Codec::KlvAsync, 0x06, mpegts::FourCc(*b"KLVA"))
    );
    assert_invariant(&made.ts, &r.out, &skip_pids(&r), false);
}

#[test]
fn output_does_not_depend_on_chunking_or_input_style() {
    let made = Synth {
        b_frames: true,
        ..Synth::default()
    }
    .make();
    let cfg = InjectConfig::default();
    let whole = try_inject(cfg.clone(), &made.ts, Feed::Reordered, made.ts.len()).unwrap();
    for chunk in [1, 187, 188, 7 * 188, 65_536] {
        let r = try_inject(cfg.clone(), &made.ts, Feed::Reordered, chunk).unwrap();
        assert!(r.out == whole.out, "chunk {chunk}");
    }
    // Pre-timed input through push_packet gives the same stream.
    let mut inj = Injector::new(cfg).unwrap();
    let mut timeline = PcrTimeline::new(None, 10_000_000);
    let mut out = Vec::new();
    let mut reorder = FrameReorder::new(16);
    let mut timed = Vec::new();
    for p in made.ts.chunks(188) {
        timeline.push(p.try_into().unwrap(), &mut |t| timed.push(t));
    }
    timeline.flush(&mut |t| timed.push(t));
    for t in &timed {
        inj.push_packet_with(
            &t.packet,
            t.clock,
            &mut out,
            &mut |f: &Frame, u: &mut Units<'_>| {
                reorder.push(*f);
                while let Some(r) = reorder.pop() {
                    u.insert(InsertUnit::sync(r.pts, unit_for(r.pts))).unwrap();
                }
            },
        )
        .unwrap();
    }
    inj.finish(&mut out).unwrap();
    assert!(out == whole.out, "push_packet output differs");
    // Mixing the two styles is refused.
    let mut inj = Injector::new(InjectConfig::default()).unwrap();
    inj.push(&made.ts[..188], &mut Vec::new()).unwrap();
    assert!(matches!(
        inj.push_packet(&[0x47; 188], None, &mut Vec::new()),
        Err(InjectError::Usage(_))
    ));
}

#[test]
fn clock_and_target_are_visible_to_the_scheduler() {
    let made = Synth::default().make();
    let mut inj = Injector::new(InjectConfig::default()).unwrap();
    assert!(inj.clock().is_none() && inj.target().is_none());
    let mut out = Vec::new();
    let mut seen = Vec::new();
    inj.push_with(&made.ts, &mut out, &mut |f: &Frame, u: &mut Units<'_>| {
        seen.push((
            f.clock,
            u.clock(),
            u.target().map(|t| t.klv_pid),
            u.carriage(),
            u.queued(),
        ));
    })
    .unwrap();
    assert!(!seen.is_empty());
    for (fc, uc, pid, carriage, queued) in &seen[1..] {
        assert!(fc.is_some() && fc == uc);
        assert_eq!(
            (*pid, *carriage, *queued),
            (Some(0x101), KlvCarriage::Sync, 0)
        );
    }
    let clock = inj.clock().unwrap();
    // The clock of the last packet processed; the internal timeline holds
    // the packets after the last PCR.
    let last_pcr = made
        .ts
        .chunks(188)
        .filter_map(|p| PacketHeader::parse(p.try_into().unwrap()).unwrap().pcr())
        .last()
        .unwrap();
    assert_eq!(clock, last_pcr);
}

// ---------------------------------------------------------------- timing

#[test]
fn b_frames_get_metadata_in_presentation_order() {
    let made = Synth {
        b_frames: true,
        frames: 120,
        ..Synth::default()
    }
    .make();
    let cfg = InjectConfig::default();
    let r = inject(cfg.clone(), &made.ts, Feed::Reordered);
    let c = checked(&r, &cfg);
    // Every frame released by the reorder buffer got its unit; only the
    // last P frame is still waiting at the end.
    assert_eq!(r.inserted.len(), 119);
    assert_eq!(c.units.len(), 119);
    let pts: Vec<u64> = r.inserted.iter().map(|(p, _)| p.unwrap()).collect();
    assert!(
        pts.windows(2).all(|w| w[1] == w[0] + FRAME_TICKS),
        "presentation order"
    );
    let frame_pts: Vec<u64> = made.frames.iter().map(|f| f.0).collect();
    assert!(
        pts.iter().all(|p| frame_pts.contains(p)),
        "each KLV PTS is a frame PTS (R-ST1402TIME-22)"
    );
    assert_eq!(read_back(&r.out, r.target.klv_pid), r.inserted);
    assert_eq!(r.stats.dropped_late, 0);
}

/// end_input_with processes what is still held without ending the stream,
/// so the frames a reorder buffer keeps until the end still get their
/// metadata before finish.
#[test]
fn end_input_lets_the_last_frames_get_metadata() {
    let made = Synth {
        b_frames: true,
        frames: 120,
        ..Synth::default()
    }
    .make();
    let cfg = InjectConfig::default();
    let mut inj = Injector::new(cfg.clone()).unwrap();
    let mut out = Vec::new();
    let mut reorder = FrameReorder::new(16);
    let mut inserted = Vec::new();
    let mut on_frame = |f: &Frame, u: &mut Units<'_>| {
        reorder.push(*f);
        while let Some(r) = reorder.pop() {
            let data = unit_for(r.pts);
            u.insert(InsertUnit::sync(r.pts, data.clone())).unwrap();
            inserted.push((Some(r.pts), data));
        }
    };
    for c in made.ts.chunks(1316) {
        inj.push_with(c, &mut out, &mut on_frame).unwrap();
    }
    inj.end_input_with(&mut out, &mut on_frame).unwrap();
    assert!(matches!(
        inj.push(&made.ts[..188], &mut out),
        Err(InjectError::Usage(_))
    ));
    for r in reorder.flush().collect::<Vec<_>>() {
        let data = unit_for(r.pts);
        inj.insert(InsertUnit::sync(r.pts, data.clone())).unwrap();
        inserted.push((Some(r.pts), data));
    }
    let stats = inj.finish(&mut out).unwrap();
    assert_eq!(inserted.len(), 120, "every frame, the last P frame too");
    assert_eq!(stats.units_inserted, 120);
    assert_eq!(stats.dropped_late, 0);
    let klv_pid = inj.target().unwrap().klv_pid;
    assert_eq!(read_back(&out, klv_pid), inserted);
}

#[test]
fn pts_wrap_is_not_a_discontinuity() {
    // Starts 1.5 s before the 2^33 wrap.
    let made = Synth {
        first_dts: PTS_WRAP - 135_000,
        b_frames: true,
        frames: 150,
        ..Synth::default()
    }
    .make();
    let cfg = InjectConfig::default();
    let r = inject(cfg.clone(), &made.ts, Feed::Reordered);
    checked(&r, &cfg);
    assert_eq!(read_back(&r.out, r.target.klv_pid), r.inserted);
    assert!(
        r.inserted.iter().any(|(p, _)| p.unwrap() < 90_000)
            && r.inserted
                .iter()
                .any(|(p, _)| p.unwrap() > PTS_WRAP - 90_000)
    );
    assert_eq!(
        (r.stats.dropped_late, r.stats.dropped_discontinuity),
        (0, 0)
    );
    assert!(!r
        .events
        .iter()
        .any(|e| matches!(e, InjectEvent::Discontinuity { .. })));
}

#[test]
fn pcr_discontinuity_drops_units_of_the_old_time_base() {
    // The clock and timestamps jump back 10 s at frame 45.
    let made = Synth {
        discontinuity_at: Some(45),
        jump: -10 * 27_000_000,
        b_frames: true,
        ..Synth::default()
    }
    .make();
    let cfg = InjectConfig::default();
    let r = inject(cfg.clone(), &made.ts, Feed::Reordered);
    checked(&r, &cfg);
    assert!(r
        .events
        .iter()
        .any(|e| matches!(e, InjectEvent::Discontinuity { .. })));
    let flagged: Vec<usize> = r
        .frames
        .iter()
        .enumerate()
        .filter(|(_, f)| f.discontinuity)
        .map(|(i, _)| i)
        .collect();
    assert_eq!(
        flagged,
        [45],
        "the first frame after the discontinuity is flagged"
    );
    // No KLV of the old time base after the discontinuity, none of the new
    // one before it (R-ISO13818-031).
    let packets: Vec<&[u8]> = r.out.chunks(188).collect();
    let disc_at = packets
        .iter()
        .position(|p| {
            let h = PacketHeader::parse((*p).try_into().unwrap()).unwrap();
            h.discontinuity() && h.pcr().is_some()
        })
        .unwrap();
    let new_base = made.frames[45].0;
    for (i, p) in packets.iter().enumerate() {
        if pid_of(p) == r.target.klv_pid && p[1] & 0x40 != 0 {
            let pts = pes_pts(p);
            let new = mpegts::pts_delta(pts, new_base) >= 0
                && mpegts::pts_delta(pts, new_base) < 90_000 * 5;
            assert_eq!(
                new,
                i > disc_at,
                "KLV PTS {pts} at packet {i}, discontinuity at {disc_at}"
            );
        }
    }
    let got = read_back(&r.out, r.target.klv_pid);
    assert!(got.len() + 4 >= r.inserted.len() && got.len() < r.inserted.len() + 1);
}

#[test]
fn sparse_input_psi_gets_copies_every_interval() {
    // PAT and PMT once a second in the input (D-ISO13818-20 keeps that
    // cadence; X-3 adds copies).
    let made = Synth {
        psi_every: 30,
        frames: 150,
        ..Synth::default()
    }
    .make();
    for interval in [100, 40] {
        let cfg = InjectConfig::default().with_psi_interval_ms(interval);
        let r = inject(cfg.clone(), &made.ts, Feed::PerFrame);
        let c = checked(&r, &cfg);
        assert!(
            c.pat_gap <= u64::from(interval) * 27_000 && c.pmt_gap <= u64::from(interval) * 27_000
        );
        assert!(r.stats.psi_extra_packets > 0);
    }
}

#[test]
fn program_without_video_takes_units_as_the_window_opens() {
    // A program whose only stream is audio: units are queued ahead using
    // the clock and go out when their window opens.
    let mut ts = Ts::new();
    let (audio, pmt_pid) = (0x210, 0x1010);
    let mut out = Vec::new();
    let mut inj = Injector::new(InjectConfig::default()).unwrap();
    let mut next_pts: Option<u64> = None;
    let mut inserted = 0;
    for n in 0..200u64 {
        if n % 20 == 0 {
            ts.section(0, &pat(1, 0, &[(1, pmt_pid)]));
            ts.section(pmt_pid, &pmt(1, 0, audio, &[], &[(0x0F, audio, vec![])]));
        }
        let clock = 27_000_000 + n * 540_000; // 20 ms per step
        ts.pes(
            audio,
            0xC0,
            Some(clock / 300 + 18_000),
            &[0xFF, 0xF1, 0x50, 0x80, 0x02, 0x1F, 0xFC],
        );
        ts.packet_with(
            audio,
            harness::common::Opts {
                pcr: Some(clock),
                no_payload: true,
                ..Default::default()
            },
            &[],
            false,
        );
        inj.push(&ts.out, &mut out).unwrap();
        ts.out.clear();
        if let Some(c) = inj.clock() {
            // Keep 500 ms of units queued, one every 100 ms.
            let horizon = c / 300 + 45_000;
            let mut p = *next_pts.get_or_insert(c / 300 + 9_000);
            while p < horizon {
                inj.insert(InsertUnit::sync(p, unit_for(p))).unwrap();
                inserted += 1;
                p += 9_000;
            }
            next_pts = Some(p);
        }
    }
    let stats = inj.finish(&mut out).unwrap();
    let t = inj.target().unwrap().clone();
    assert_eq!((t.video_pid, t.klv_pid), (None, 0x211));
    let c = check(
        &out,
        &Expect {
            program: 1,
            pmt_pid,
            pcr_pid: Some(audio),
            klv_pid: 0x211,
            sync: true,
            service_id: Some(0),
            psi_interval_ms: 100,
            min_lead_ms: 1,
            max_lead_ms: 990,
            merged: false,
        },
    );
    assert!(c.errors.is_empty(), "{:#?}", c.errors);
    assert!(stats.units_inserted as usize + stats.dropped_unplaced as usize == inserted);
    assert!(stats.units_inserted > 30);
    assert_eq!(stats.dropped_late, 0);
}

// ---------------------------------------------------------------- programs and PSI changes

#[test]
fn multi_program_input() {
    let mut s = Synth {
        frames: 60,
        packets_per_frame: 60,
        ..Synth::default()
    };
    s.programs.push(Prog::new(2, 0x1001, 0x200));
    let made = s.make();
    // Auto: the first program with video.
    let cfg = InjectConfig::default();
    let r = inject(cfg.clone(), &made.ts, Feed::PerFrame);
    assert_eq!(r.target.program, 1);
    checked(&r, &cfg);
    assert!(r.events.iter().any(|e| matches!(
        e,
        InjectEvent::Warning(InjectWarning::MultiProgramVbr { programs: 2 })
    )));
    // Program 2's PMT went through unchanged.
    let p2_in = pmt_sections(&made.ts, 0x1001);
    assert!(pmt_sections(&r.out, 0x1001).iter().all(|s| s == &p2_in[0]));
    assert_invariant(&made.ts, &r.out, &skip_pids(&r), false);
    // By number.
    let cfg = InjectConfig::default().with_program(ProgramSelect::Number(2));
    let r = inject(cfg.clone(), &made.ts, Feed::PerFrame);
    assert_eq!(
        (r.target.program, r.target.pmt_pid, r.target.klv_pid),
        (2, 0x1001, 0x201)
    );
    checked(&r, &cfg);
    assert_eq!(read_back(&r.out, 0x201), r.inserted);
    // A program that does not exist.
    let cfg = InjectConfig::default().with_program(ProgramSelect::Number(9));
    assert!(matches!(
        try_inject(cfg, &made.ts, Feed::Nothing, 1316),
        Err(InjectError::NoTarget { .. })
    ));
}

#[test]
fn pmt_pid_change_mid_stream() {
    let made = Synth {
        pmt_pid_change: Some((40, 0x1100)),
        frames: 90,
        ..Synth::default()
    }
    .make();
    let cfg = InjectConfig::default();
    let r = inject(cfg.clone(), &made.ts, Feed::PerFrame);
    assert_eq!(
        (r.target.pmt_pid, r.target.klv_pid),
        (0x1100, 0x101),
        "the KLV PID stays"
    );
    // The checker follows the new PMT PID; the KLV stream continues.
    let e = expect(&r, &cfg);
    let c = check(&r.out, &e);
    assert!(c.errors.is_empty(), "{:#?}", c.errors);
    let before: Vec<Vec<u8>> = pmt_sections(&r.out, 0x1100);
    assert!(!before.is_empty());
    assert_eq!(read_back(&r.out, 0x101).len(), 90);
    let report = mpegts::analyze(&r.out);
    assert_eq!(report.program(1).unwrap().pmt_pid, 0x1100);
    assert_eq!(report.stream(0x101).unwrap().codec, Codec::KlvSync);
}

#[test]
fn input_that_starts_using_our_pid_is_a_collision() {
    let made = Synth {
        frames: 30,
        ..Synth::default()
    }
    .make();
    let mut ts = made.ts.clone();
    // Ten frames in, the input sends a packet on 0x101.
    let mut p = [0xFFu8; 188];
    p[..4].copy_from_slice(&[0x47, 0x01, 0x01, 0x10]);
    ts.splice(188 * 400..188 * 400, p);
    let err = try_inject(InjectConfig::default(), &ts, Feed::PerFrame, 1316)
        .err()
        .unwrap();
    assert_eq!(
        err,
        InjectError::PidInUse {
            pid: 0x101,
            reason: "the input started using the KLV PID"
        }
    );
    assert!(err.to_string().contains("0x0101"));
}

// ---------------------------------------------------------------- existing KLV

#[test]
fn keep_merges_into_the_existing_sync_stream() {
    let prog = Prog::new(1, 0x1000, 0x100).with(Existing::SyncKlv {
        pid: 0x101,
        service: 0,
    });
    let made = Synth {
        programs: vec![prog],
        b_frames: true,
        ..Synth::default()
    }
    .make();
    let cfg = InjectConfig::default();
    let r = inject(cfg.clone(), &made.ts, Feed::Reordered);
    assert_eq!(
        (r.target.klv_pid, r.target.service_id, r.target.merged),
        (0x101, Some(1), true)
    );
    let c = checked(&r, &cfg);
    // Both services on one PID, ours as service 1 (ST 1402-13).
    let ours: Vec<(Option<u64>, Vec<u8>)> = c
        .units
        .iter()
        .filter(|u| u.service_id == Some(1))
        .map(|u| (u.pts, u.payload.clone()))
        .collect();
    let theirs: Vec<(Option<u64>, Vec<u8>)> = c
        .units
        .iter()
        .filter(|u| u.service_id == Some(0))
        .map(|u| (u.pts, u.payload.clone()))
        .collect();
    assert_eq!(ours, r.inserted);
    let input_aus: Vec<(Option<u64>, Vec<u8>)> =
        made.klv.iter().map(|k| (k.1, k.2.clone())).collect();
    assert_eq!(
        theirs, input_aus,
        "input access unit bytes are never changed"
    );
    // CC continuous (checked by the checker and the demuxer). Cells are
    // numbered per access unit (DECISIONS X-8): ours 0, the input's as
    // they were.
    let cells = |out: &[u8]| -> Vec<(u8, u8)> {
        out.chunks(188)
            .filter(|p| pid_of(p) == 0x101 && p[1] & 0x40 != 0)
            .map(|p| {
                let h = PacketHeader::parse(p.try_into().unwrap()).unwrap();
                let pes = h.payload(p.try_into().unwrap());
                (pes[14], pes[15])
            })
            .collect()
    };
    let input_seqs: Vec<u8> = cells(&made.ts).iter().map(|c| c.1).collect();
    let out = cells(&r.out);
    assert!(out.iter().filter(|c| c.0 == 1).all(|c| c.1 == 0), "{out:?}");
    let kept: Vec<u8> = out.iter().filter(|c| c.0 == 0).map(|c| c.1).collect();
    assert_eq!(kept, input_seqs, "the input's cells keep their numbers");
    // Numbered per stream, the input's cells are renumbered so that one
    // counter runs over both services.
    let mut per_stream = cfg.clone();
    per_stream.sync.cell_numbering = mpegts::mux::CellNumbering::PerStream;
    let r2 = inject(per_stream.clone(), &made.ts, Feed::Reordered);
    checked(&r2, &per_stream);
    let seqs: Vec<u8> = cells(&r2.out).iter().map(|c| c.1).collect();
    assert!(
        seqs.windows(2).all(|w| w[1] == w[0].wrapping_add(1)),
        "{seqs:?}"
    );
    assert_eq!(seqs[0], 0, "continues the input's numbering");
    let report = mpegts::analyze(&r.out);
    let tags: Vec<u8> = report
        .stream(0x101)
        .unwrap()
        .descriptors
        .iter()
        .map(|d| d.tag)
        .collect();
    assert_eq!(
        tags,
        [0x26, 0x26, 0x27],
        "our metadata_descriptor added, one metadata_STD_descriptor kept"
    );
    assert_eq!(
        report.streams().filter(|s| s.stream_type == 0x15).count(),
        1,
        "no second 0x15 stream"
    );
    // Everything but PSI and the merged PID is untouched.
    assert_invariant(&made.ts, &r.out, &skip_pids(&r), false);
}

#[test]
fn keep_adds_async_on_a_new_pid_next_to_existing_klv() {
    let prog = Prog::new(1, 0x1000, 0x100).with(Existing::AsyncKlv { pid: 0x101 });
    let made = Synth {
        programs: vec![prog],
        ..Synth::default()
    }
    .make();
    for carriage in [KlvCarriage::Async, KlvCarriage::Sync] {
        let cfg = InjectConfig::default().with_carriage(carriage);
        let feed = if carriage == KlvCarriage::Sync {
            Feed::PerFrame
        } else {
            Feed::Async
        };
        let r = inject(cfg.clone(), &made.ts, feed);
        assert_eq!((r.target.klv_pid, r.target.merged), (0x102, false));
        checked(&r, &cfg);
        assert_eq!(read_back(&r.out, 0x102), r.inserted);
        let theirs: Vec<Vec<u8>> = read_back(&r.out, 0x101)
            .into_iter()
            .map(|(_, d)| d)
            .collect();
        assert_eq!(
            theirs,
            made.klv.iter().map(|k| k.2.clone()).collect::<Vec<_>>()
        );
        assert_invariant(&made.ts, &r.out, &skip_pids(&r), false);
    }
}

#[test]
fn replace_removes_the_input_klv_and_reuses_its_pid() {
    let prog = Prog::new(1, 0x1000, 0x100).with(Existing::AsyncKlv { pid: 0x105 });
    let made = Synth {
        programs: vec![prog],
        ..Synth::default()
    }
    .make();
    let cfg = InjectConfig::default().with_existing_klv(ExistingKlv::Replace);
    let r = inject(cfg.clone(), &made.ts, Feed::PerFrame);
    assert_eq!(
        (r.target.klv_pid, &r.target.stripped[..]),
        (0x105, &[0x105][..])
    );
    checked(&r, &cfg);
    assert_eq!(
        read_back(&r.out, 0x105),
        r.inserted,
        "only our units on the PID"
    );
    assert_eq!(r.stats.stripped_packets, 90);
    let report = mpegts::analyze(&r.out);
    let streams: Vec<(u16, Codec)> = report
        .program(1)
        .unwrap()
        .streams
        .iter()
        .map(|s| (s.pid, s.codec))
        .collect();
    assert_eq!(streams, [(0x100, Codec::H264), (0x105, Codec::KlvSync)]);
    assert_invariant(&made.ts, &r.out, &skip_pids(&r), false);
}

#[test]
fn replace_keeps_a_pcr_carried_on_the_removed_pid() {
    // The program's PCR rides in the KLV packets.
    let mut prog = Prog::new(1, 0x1000, 0x100).with(Existing::SyncKlv {
        pid: 0x105,
        service: 3,
    });
    prog.pcr_pid = 0x105;
    let made = Synth {
        programs: vec![prog],
        ..Synth::default()
    }
    .make();
    let cfg = InjectConfig::default().with_existing_klv(ExistingKlv::Replace);
    let r = inject(cfg.clone(), &made.ts, Feed::PerFrame);
    // Never our KLV on a PCR-bearing PID: a new PID instead.
    assert_eq!((r.target.klv_pid, r.target.pcr_pid), (0x106, Some(0x105)));
    // The removed service's id is free again (D-ISO13818-03).
    assert_eq!(r.target.service_id, Some(0));
    checked(&r, &cfg);
    assert_eq!(r.stats.pcr_only_packets, 90);
    // Every PCR survives with its value, in adaptation-field-only packets
    // with a constant counter (R-ISO13818-010).
    let pcrs = |ts: &[u8]| -> Vec<u64> {
        ts.chunks(188)
            .filter_map(|p| PacketHeader::parse(p.try_into().unwrap()).unwrap().pcr())
            .collect()
    };
    assert_eq!(pcrs(&r.out), pcrs(&made.ts));
    let on_105: Vec<PacketHeader> = r
        .out
        .chunks(188)
        .filter(|p| pid_of(p) == 0x105)
        .map(|p| PacketHeader::parse(p.try_into().unwrap()).unwrap())
        .collect();
    assert!(on_105
        .iter()
        .all(|h| h.adaptation_field_control == 0b10 && h.pcr().is_some()));
    assert!(on_105
        .windows(2)
        .all(|w| w[0].continuity_counter == w[1].continuity_counter));
    assert_eq!(read_back(&r.out, 0x106), r.inserted);
    let report = mpegts::analyze(&r.out);
    assert!(report.pids.iter().all(|p| p.cc_errors == 0));
    assert_eq!(report.program(1).unwrap().pcr_pid, Some(0x105));
}

#[test]
fn fail_refuses_a_program_with_klv() {
    let prog = Prog::new(1, 0x1000, 0x100).with(Existing::SyncKlv {
        pid: 0x101,
        service: 0,
    });
    let made = Synth {
        programs: vec![prog],
        frames: 10,
        ..Synth::default()
    }
    .make();
    let cfg = InjectConfig::default().with_existing_klv(ExistingKlv::Fail);
    let err = try_inject(cfg, &made.ts, Feed::Nothing, 1316)
        .err()
        .unwrap();
    assert_eq!(
        err,
        InjectError::ExistingKlv {
            program: 1,
            pid: 0x101
        }
    );
    // Without KLV in the input, Fail is no obstacle.
    let made = Synth {
        frames: 10,
        ..Synth::default()
    }
    .make();
    assert!(try_inject(
        InjectConfig::default().with_existing_klv(ExistingKlv::Fail),
        &made.ts,
        Feed::Nothing,
        188
    )
    .is_ok());
}

// ---------------------------------------------------------------- refusals

#[test]
fn fixed_pid_and_service_id() {
    let made = Synth {
        frames: 20,
        ..Synth::default()
    }
    .make();
    let cfg = InjectConfig::default().with_pid(0x0777).with_service_id(9);
    let r = inject(cfg.clone(), &made.ts, Feed::PerFrame);
    assert_eq!((r.target.klv_pid, r.target.service_id), (0x777, Some(9)));
    checked(&r, &cfg);
    for (pid, why) in [
        (0x100, "it is the program's PCR_PID"),
        (0x1000, "the input uses it"),
        (0x1FFF, ""),
    ] {
        let r = try_inject(
            InjectConfig::default().with_pid(pid),
            &made.ts,
            Feed::Nothing,
            1316,
        );
        match r {
            Err(InjectError::PidInUse { pid: p, reason }) => assert_eq!((p, reason), (pid, why)),
            Err(InjectError::InvalidConfig(_)) => assert_eq!(pid, 0x1FFF),
            other => panic!("PID 0x{pid:X}: {:?}", other.map(|r| r.target)),
        }
    }
    // A service id another stream uses.
    let prog = Prog::new(1, 0x1000, 0x100).with(Existing::SyncKlv {
        pid: 0x101,
        service: 4,
    });
    let made = Synth {
        programs: vec![prog],
        frames: 10,
        ..Synth::default()
    }
    .make();
    let cfg = InjectConfig::default().with_service_id(4);
    assert_eq!(
        try_inject(cfg, &made.ts, Feed::Nothing, 1316).err(),
        Some(InjectError::ServiceIdInUse(4))
    );
}

#[test]
fn scrambled_target_is_refused() {
    let made = Synth {
        scrambled: true,
        frames: 10,
        ..Synth::default()
    }
    .make();
    let err = try_inject(InjectConfig::default(), &made.ts, Feed::Nothing, 1316)
        .err()
        .unwrap();
    assert_eq!(
        err,
        InjectError::Scrambled {
            program: 1,
            pid: 0x100
        }
    );
}

#[test]
fn no_target_within_the_hold_limit() {
    let made = Synth {
        frames: 10,
        ..Synth::default()
    }
    .make();
    // Video without any PSI.
    let no_psi: Vec<u8> = made
        .ts
        .chunks(188)
        .filter(|p| pid_of(p) != 0 && pid_of(p) != 0x1000)
        .flatten()
        .copied()
        .collect();
    let cfg = InjectConfig::default().with_max_held_packets(100);
    let mut inj = Injector::new(cfg).unwrap();
    let err = inj.push(&no_psi, &mut Vec::new()).err().unwrap();
    assert_eq!(err, InjectError::NoTarget { held: 100 });
    // The error sticks.
    assert_eq!(inj.push(&no_psi, &mut Vec::new()).err(), Some(err.clone()));
    assert_eq!(inj.finish(&mut Vec::new()).err(), Some(err));
    // Within the limit but never found: at the end.
    let mut inj = Injector::new(InjectConfig::default()).unwrap();
    inj.push(&no_psi, &mut Vec::new()).unwrap();
    assert!(matches!(
        inj.finish(&mut Vec::new()),
        Err(InjectError::NoTarget { .. })
    ));
}

#[test]
fn unit_rules_and_queue_limits() {
    let mut inj = Injector::new(InjectConfig::default().with_queue_limits(3, 1000)).unwrap();
    assert!(matches!(
        inj.insert(InsertUnit::asynchronous(vec![1])),
        Err(InjectError::InvalidUnit(_))
    ));
    assert!(matches!(
        inj.insert(InsertUnit::sync(1, vec![])),
        Err(InjectError::InvalidUnit(_))
    ));
    assert!(matches!(
        inj.insert(InsertUnit::sync(1, vec![0; 1001])),
        Err(InjectError::InvalidUnit(_))
    ));
    for pts in [300, 100, 200] {
        inj.insert(InsertUnit::sync(pts, vec![1; 10])).unwrap();
    }
    assert_eq!(
        inj.insert(InsertUnit::sync(400, vec![1; 10])),
        Err(InjectError::QueueFull {
            units: 3,
            bytes: 30
        })
    );
    assert_eq!(inj.stats().units_queued, 3);
    // Refused units do not end the injector.
    assert!(inj.push(&[], &mut Vec::new()).is_ok());

    let mut inj = Injector::new(InjectConfig::default().with_carriage(KlvCarriage::Async)).unwrap();
    assert!(matches!(
        inj.insert(InsertUnit::sync(1, vec![1])),
        Err(InjectError::InvalidUnit(_))
    ));
    inj.insert(InsertUnit::asynchronous(vec![1])).unwrap();

    // Out of order once a later PTS has been sent.
    let made = Synth {
        frames: 30,
        ..Synth::default()
    }
    .make();
    let mut inj = Injector::new(InjectConfig::default()).unwrap();
    let mut out = Vec::new();
    let mut last = 0;
    inj.push_with(&made.ts, &mut out, &mut |f: &Frame, u: &mut Units<'_>| {
        u.insert(InsertUnit::sync(f.pts, unit_for(f.pts))).unwrap();
        last = f.pts;
    })
    .unwrap();
    let early = last - 10 * FRAME_TICKS;
    // The unit for the last frame went out right before it.
    assert_eq!(
        inj.insert(InsertUnit::sync(early, vec![1])),
        Err(InjectError::OutOfOrder { pts: early, last })
    );

    // Configuration checks.
    for cfg in [
        InjectConfig::default().with_psi_interval_ms(0),
        InjectConfig::default().with_psi_interval_ms(250),
        InjectConfig::default().with_lead_ms(0, 990),
        InjectConfig::default().with_lead_ms(1, 1001),
        InjectConfig::default().with_lead_ms(500, 400),
        InjectConfig::default().with_queue_limits(0, 1),
        InjectConfig::default()
            .with_carriage(KlvCarriage::Async)
            .with_service_id(1),
        InjectConfig::default().with_program(ProgramSelect::Number(0)),
        InjectConfig::default().with_pid(0x000F),
    ] {
        assert!(matches!(
            Injector::new(cfg),
            Err(InjectError::InvalidConfig(_))
        ));
    }
}

#[test]
fn late_units_are_dropped_and_counted() {
    let made = Synth {
        frames: 40,
        ..Synth::default()
    }
    .make();
    let mut inj = Injector::new(InjectConfig::default()).unwrap();
    let mut out = Vec::new();
    inj.push_with(&made.ts, &mut out, &mut |f: &Frame, u: &mut Units<'_>| {
        // A unit for a PTS already in the past.
        let _ = u.insert(InsertUnit::sync(f.clock.unwrap_or(0) / 300, vec![1; 20]));
    })
    .unwrap();
    let stats = inj.finish(&mut out).unwrap();
    assert!(stats.dropped_late > 30, "{stats:?}");
    assert_eq!(stats.units_inserted, 0);
    let events: Vec<InjectEvent> = std::iter::from_fn(|| inj.pop_event()).collect();
    assert!(events.iter().any(|e| matches!(
        e,
        InjectEvent::Dropped {
            reason: DropReason::Late,
            ..
        }
    )));
}

// ---------------------------------------------------------------- CBR

fn cbr(on_overflow: OnOverflow) -> InjectConfig {
    InjectConfig::default().with_rate(RateMode::Cbr { on_overflow })
}

#[test]
fn cbr_keeps_every_packet_in_its_slot() {
    let made = Synth {
        b_frames: true,
        ..Synth::default()
    }
    .make();
    let cfg = cbr(OnOverflow::Fail);
    let r = inject(cfg.clone(), &made.ts, Feed::Reordered);
    assert_eq!(r.out.len(), made.ts.len(), "one packet out per packet in");
    checked(&r, &cfg);
    assert_eq!(read_back(&r.out, r.target.klv_pid), r.inserted);
    // Every packet that is not a free slot (null, PAT, PMT) is at its
    // index, byte for byte; so every PCR is too (D-ISO13818-15).
    let mut pcrs = 0;
    for (i, (a, b)) in made.ts.chunks(188).zip(r.out.chunks(188)).enumerate() {
        let pid = pid_of(a);
        if pid != 0x1FFF && pid != 0 && pid != 0x1000 {
            assert_eq!(a, b, "packet {i}");
        }
        pcrs += usize::from(
            PacketHeader::parse(a.try_into().unwrap())
                .unwrap()
                .pcr()
                .is_some(),
        );
    }
    assert_eq!(pcrs, 90);
    assert_eq!(
        r.stats.slots_filled,
        r.stats.klv_packets + r.stats.psi_packets
    );
    assert!(r.stats.free_bitrate > 0 && r.stats.null_bitrate > 0);
    assert_invariant(&made.ts, &r.out, &skip_pids(&r), true);
}

#[test]
fn cbr_without_headroom_fails_loudly() {
    // No null packets at all: every slot is video or PSI (PSI every frame,
    // so the PSI slots go to PSI).
    let made = Synth {
        packets_per_frame: 19,
        video_bytes: 3100,
        psi_every: 1,
        ..Synth::default()
    }
    .make();
    assert_eq!(
        made.ts.chunks(188).filter(|p| pid_of(p) == 0x1FFF).count(),
        0
    );
    let err = try_inject(cbr(OnOverflow::Fail), &made.ts, Feed::PerFrame, 1316)
        .err()
        .unwrap();
    assert!(matches!(err, InjectError::CbrOverflow { .. }), "{err:?}");
    assert!(err.to_string().contains("CBR"));
}

#[test]
fn cbr_drop_oldest_drops_and_counts() {
    // One null packet per frame, three-packet units every frame.
    let made = Synth {
        packets_per_frame: 20,
        video_bytes: 3100,
        psi_every: 1,
        ..Synth::default()
    }
    .make();
    let cfg = cbr(OnOverflow::DropOldest);
    let r = inject(cfg.clone(), &made.ts, Feed::Big);
    assert_eq!(r.out.len(), made.ts.len());
    assert!(r.stats.dropped_overflow > 0, "{:?}", r.stats);
    assert_eq!(
        r.stats.units_inserted
            + r.stats.dropped_overflow
            + r.stats.dropped_late
            + r.stats.dropped_unplaced,
        90
    );
    let c = checked(&r, &cfg);
    // A unit still going out when the input ends has no slots left.
    assert_eq!(c.cut_tail, r.stats.dropped_unplaced > 0);
    let got = read_back(&r.out, r.target.klv_pid);
    assert_eq!(got.len() as u64, r.stats.units_inserted);
    assert!(got.iter().all(|u| r.inserted.contains(u)));
}

#[test]
fn cbr_merge_and_replace() {
    let prog = Prog::new(1, 0x1000, 0x100).with(Existing::SyncKlv {
        pid: 0x101,
        service: 0,
    });
    let made = Synth {
        programs: vec![prog],
        b_frames: true,
        ..Synth::default()
    }
    .make();
    for existing in [ExistingKlv::Keep, ExistingKlv::Replace] {
        let cfg = cbr(OnOverflow::Fail).with_existing_klv(existing);
        let r = inject(cfg.clone(), &made.ts, Feed::Reordered);
        assert_eq!(r.out.len(), made.ts.len());
        let c = checked(&r, &cfg);
        let ours: Vec<(Option<u64>, Vec<u8>)> = c
            .units
            .iter()
            .filter(|u| u.service_id == r.target.service_id)
            .map(|u| (u.pts, u.payload.clone()))
            .collect();
        assert_eq!(ours, r.inserted);
    }
}

// ---------------------------------------------------------------- handmade PSI

#[test]
fn shared_pmt_pid_and_next_sections() {
    // Two programs share PMT PID 0x1000; program 1 also sends a "next"
    // PMT section (current_next_indicator 0).
    let mut ts = Ts::new();
    let p1 = pmt(1, 4, 0x100, &[], &[(H264, 0x100, vec![])]);
    let p2 = pmt(
        2,
        7,
        0x200,
        &[],
        &[
            (H264, 0x200, vec![]),
            (METADATA, 0x201, metadata_descriptor(b"KLVA")),
        ],
    );
    let mut next = harness::common::section_full(0x02, 1, 5, false, 0, 0, &p1[8..p1.len() - 4]);
    next[5] &= !1;
    let crc = mpegts::crc32_mpeg2(&next[..next.len() - 4]);
    let n = next.len();
    next[n - 4..].copy_from_slice(&crc.to_be_bytes());
    for i in 0..40u64 {
        if i % 4 == 0 {
            ts.section(0, &pat(1, 0, &[(1, 0x1000), (2, 0x1000)]));
            ts.section(0x1000, &p1);
            ts.section(0x1000, &p2);
            ts.section(0x1000, &next);
        }
        let opts = harness::common::Opts {
            pcr: Some(27_000_000 + i * 900_000),
            ..Default::default()
        };
        let mut pes = vec![0, 0, 0, 1, 9, 0xF0];
        pes.resize(500, 0x11);
        send_pes(
            &mut ts,
            0x100,
            opts,
            &harness::common::pes_bytes(0xE0, Some(90_000 + i * 3003 + 36_000), None, &pes),
        );
    }
    let cfg = InjectConfig::default().with_program(ProgramSelect::Number(1));
    let r = inject(cfg.clone(), &ts.out, Feed::PerFrame);
    // Service 0 is taken by program 2's metadata stream.
    assert_eq!((r.target.klv_pid, r.target.service_id), (0x101, Some(1)));
    checked(&r, &cfg);
    let sections = pmt_sections(&r.out, 0x1000);
    assert!(
        sections.iter().any(|s| s == &p2),
        "program 2's PMT is copied"
    );
    let next_out: Vec<&Vec<u8>> = sections.iter().filter(|s| s[5] & 1 == 0).collect();
    assert!(!next_out.is_empty());
    // The "next" section is rewritten with the same mapping: version 5 -> 6.
    assert!(next_out
        .iter()
        .all(|s| (s[5] >> 1) & 0x1F == 6 && s.len() > next.len()));
    assert_eq!(mpegts::analyze(&r.out).program(1).unwrap().version, Some(5));
    let _ = (KLV_KEY, PRIVATE);
}

#[test]
fn pmt_that_cannot_grow_is_refused() {
    // A PMT whose program_info nearly fills the section.
    let mut ts = Ts::new();
    let mut info = Vec::new();
    while info.len() < 990 {
        info.extend_from_slice(&[0x80, 0x30]);
        info.extend_from_slice(&[0xAB; 0x30]);
    }
    ts.section(0, &pat(1, 0, &[(1, 0x1000)]));
    ts.section(0x1000, &pmt(1, 0, 0x100, &info, &[(H264, 0x100, vec![])]));
    let err = try_inject(InjectConfig::default(), &ts.out, Feed::Nothing, 188)
        .err()
        .unwrap();
    assert!(
        matches!(err, InjectError::SectionTooLong { section_length } if section_length > 1021),
        "{err:?}"
    );
}

#[test]
fn reports_and_counters_add_up() {
    let made = Synth::default().make();
    let r = inject(InjectConfig::default(), &made.ts, Feed::PerFrame);
    let s = r.stats;
    let pids: HashMap<u16, usize> = r.out.chunks(188).fold(HashMap::new(), |mut m, p| {
        *m.entry(pid_of(p)).or_default() += 1;
        m
    });
    assert_eq!(s.packets_in as usize, made.ts.len() / 188);
    assert_eq!(s.packets_out as usize, r.out.len() / 188);
    assert_eq!(s.klv_packets as usize, pids[&0x101]);
    assert_eq!(s.psi_packets as usize, pids[&0] + pids[&0x1000]);
    assert!(s.added_bitrate > 0 && s.metadata_buffer_peak > 0);
    assert_eq!((s.units_queued, s.held_packets, s.bytes_skipped), (0, 0, 0));
}

// ---------------------------------------------------------------- external oracles

#[path = "../examples/mux.rs"]
#[allow(dead_code)]
mod example;

/// Injected streams for the oracles: (name, output, what was inserted,
/// KLV PID, sync).
fn oracle_streams() -> Vec<(&'static str, Run, bool)> {
    // Real (tiny) H.264 so decoders have nothing to complain about, at
    // about 11 Mbit/s so the few packets VBR adds to a PCR interval stay
    // within pcrverify's 1 ms jitter limit.
    let base = Synth {
        b_frames: true,
        frames: 120,
        packets_per_frame: 240,
        video: Some(example::h264_frame),
        ..Synth::default()
    };
    let made = base.clone().make();
    let mut out = vec![
        (
            "sync",
            inject(InjectConfig::default(), &made.ts, Feed::Reordered),
            true,
        ),
        (
            "async",
            inject(
                InjectConfig::default().with_carriage(KlvCarriage::Async),
                &made.ts,
                Feed::Async,
            ),
            false,
        ),
        (
            "cbr",
            inject(cbr(OnOverflow::Fail), &made.ts, Feed::Reordered),
            true,
        ),
    ];
    let prog = Prog::new(1, 0x1000, 0x100).with(Existing::SyncKlv {
        pid: 0x101,
        service: 0,
    });
    let merged = Synth {
        programs: vec![prog],
        ..base.clone()
    }
    .make();
    out.push((
        "merge",
        inject(InjectConfig::default(), &merged.ts, Feed::Reordered),
        true,
    ));
    let mut prog = Prog::new(1, 0x1000, 0x100).with(Existing::AsyncKlv { pid: 0x105 });
    prog.pcr_pid = 0x105;
    let replaced = Synth {
        programs: vec![prog],
        ..base
    }
    .make();
    let cfg = InjectConfig::default().with_existing_klv(ExistingKlv::Replace);
    out.push(("replace", inject(cfg, &replaced.ts, Feed::Reordered), true));
    out
}

fn oracle_dir(tool: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("mpegts-inject-{tool}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// With `MPEGTS_FFPROBE` naming an ffprobe built with the KLV patch series
/// (ffmpeg-klv-fix): it reports the injected KLV stream with the right
/// profile and every injected unit as a packet with its size and PTS. With
/// `MPEGTS_FFMPEG` too, `-map 0:d -c copy -f data` returns the injected KLV
/// bytes exactly. Skipped when `MPEGTS_FFPROBE` is unset; a wrong path
/// fails.
#[test]
fn ffprobe_and_ffmpeg_read_injected_klv() {
    let Ok(ffprobe) = std::env::var("MPEGTS_FFPROBE") else {
        eprintln!("skipping: set MPEGTS_FFPROBE to a patched ffprobe");
        return;
    };
    let ffmpeg = std::env::var("MPEGTS_FFMPEG").ok();
    let dir = oracle_dir("ffprobe");
    for (name, r, sync) in oracle_streams() {
        let path = dir.join(format!("{name}.ts"));
        std::fs::write(&path, &r.out).unwrap();
        let out = std::process::Command::new(&ffprobe)
            .args([
                "-v",
                "error",
                "-of",
                "json",
                "-show_streams",
                "-show_packets",
            ])
            .arg(&path)
            .output()
            .unwrap_or_else(|e| panic!("cannot run {ffprobe}: {e}"));
        assert!(
            out.status.success(),
            "{name}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            out.stderr.is_empty(),
            "{name}: ffprobe complained: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        let streams = json["streams"].as_array().unwrap();
        let id = format!("0x{:x}", r.target.klv_pid);
        let klv = streams
            .iter()
            .find(|s| s["id"].as_str() == Some(&id))
            .unwrap_or_else(|| panic!("{name}: no stream {id}"));
        assert_eq!(klv["codec_name"], "klv", "{name}");
        assert_eq!(
            klv["profile"],
            if sync { "0" } else { "1" },
            "{name}: AV_PROFILE_KLVA_SYNC / _ASYNC"
        );
        let index = klv["index"].as_u64().unwrap();
        let packets: Vec<(usize, Option<u64>)> = json["packets"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|p| p["stream_index"].as_u64() == Some(index))
            .map(|p| {
                (
                    p["size"].as_str().unwrap().parse().unwrap(),
                    p["pts"].as_u64(),
                )
            })
            .collect();
        let ours: Vec<(usize, Option<u64>)> =
            r.inserted.iter().map(|(pts, d)| (d.len(), *pts)).collect();
        if r.target.merged {
            // Both services' access units come out as packets of the stream.
            assert!(
                ours.iter().all(|u| packets.contains(u)),
                "{name}: injected units missing"
            );
            assert_eq!(packets.len(), ours.len() + 120);
        } else {
            assert_eq!(packets, ours, "{name}: KLV packets");
        }
        if let (Some(ffmpeg), false) = (&ffmpeg, r.target.merged) {
            let klv_path = dir.join(format!("{name}.klv"));
            let status = std::process::Command::new(ffmpeg)
                .args(["-v", "error", "-y", "-i"])
                .arg(&path)
                .args(["-map", "0:d", "-c", "copy", "-f", "data"])
                .arg(&klv_path)
                .status()
                .unwrap_or_else(|e| panic!("cannot run {ffmpeg}: {e}"));
            assert!(status.success(), "{name}: ffmpeg remux failed");
            let want: Vec<u8> = r.inserted.iter().flat_map(|(_, d)| d.clone()).collect();
            assert!(
                std::fs::read(&klv_path).unwrap() == want,
                "{name}: remuxed KLV differs"
            );
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// With `MPEGTS_TSDUCK` naming a directory holding TSDuck's `tsp` and
/// `tsanalyze`: `tsp -P continuity -P pcrverify` finds no error in the
/// injected streams and `tsanalyze` lists the KLV PID. Skipped when unset;
/// a wrong path fails.
#[test]
fn tsduck_accepts_injected_streams() {
    let Ok(bin) = std::env::var("MPEGTS_TSDUCK") else {
        eprintln!("skipping: set MPEGTS_TSDUCK to the directory of tsp and tsanalyze");
        return;
    };
    let bin = std::path::PathBuf::from(bin);
    let dir = oracle_dir("tsduck");
    for (name, r, _) in oracle_streams() {
        let path = dir.join(format!("{name}.ts"));
        std::fs::write(&path, &r.out).unwrap();
        let out = std::process::Command::new(bin.join("tsp"))
            .arg("-I")
            .arg("file")
            .arg(&path)
            .args(["-P", "continuity", "-P", "pcrverify", "-O", "drop"])
            .output()
            .unwrap_or_else(|e| panic!("cannot run {}: {e}", bin.join("tsp").display()));
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "{name}: tsp failed: {stderr}");
        // Only pcrverify's summary, with no PCR over the jitter limit; any
        // other line is a continuity or PCR error.
        let summary: Vec<&str> = stderr.lines().filter(|l| l.contains("PCR OK")).collect();
        assert_eq!(summary.len(), 1, "{name}: tsp reported: {stderr}");
        assert!(
            summary[0].contains(" 0 with jitter"),
            "{name}: {}",
            summary[0]
        );
        assert_eq!(
            stderr.lines().filter(|l| !l.trim().is_empty()).count(),
            1,
            "{name}: tsp reported: {stderr}"
        );
        eprintln!("{name}: {}", summary[0]);
        let out = std::process::Command::new(bin.join("tsanalyze"))
            .arg(&path)
            .output()
            .unwrap_or_else(|e| panic!("cannot run {}: {e}", bin.join("tsanalyze").display()));
        assert!(
            out.status.success(),
            "{name}: tsanalyze failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let text = String::from_utf8_lossy(&out.stdout);
        let pid = format!("0x{:04X}", r.target.klv_pid);
        assert!(
            text.contains(&pid),
            "{name}: tsanalyze does not list {pid}:\n{text}"
        );
        for l in text.lines().filter(|l| l.contains(&pid)).take(3) {
            eprintln!("{name}: tsanalyze: {}", l.trim());
        }
        let errors: Vec<&str> = text
            .lines()
            .filter(|l| l.contains("Discontinuities") || l.contains("errors"))
            .collect();
        for l in errors {
            let n: u64 = l
                .split(':')
                .nth(1)
                .and_then(|v| v.split_whitespace().next())
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
            assert_eq!(n, 0, "{name}: tsanalyze: {l}");
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------- throughput

/// Release-mode throughput on a synthetic stream (about 85 MB of 11 Mbit/s
/// video with a KLV unit per frame):
///
///     cargo test --release --features mux --test inject -- --ignored throughput --nocapture
#[test]
#[ignore]
fn throughput() {
    let made = Synth {
        frames: 2000,
        packets_per_frame: 240,
        video_bytes: 40_000,
        b_frames: true,
        ..Synth::default()
    }
    .make();
    let mb = made.ts.len() as f64 / 1e6;
    let time = |name: &str, f: &mut dyn FnMut() -> usize| {
        let mut best = f64::MAX;
        let mut out = 0;
        for _ in 0..5 {
            let t = std::time::Instant::now();
            out = f();
            best = best.min(t.elapsed().as_secs_f64());
        }
        eprintln!(
            "{name:<40} {:>8.0} MB/s  ({:.1} MB in {:.1} ms, {} packets out)",
            mb / best,
            mb,
            best * 1e3,
            out / 188
        );
    };
    let mut sink = Vec::with_capacity(made.ts.len() * 2);
    time("demuxer (reference)", &mut || {
        let mut d = Demuxer::new();
        for c in made.ts.chunks(65_536) {
            d.push(c);
        }
        d.finish();
        made.ts.len()
    });
    for (name, cfg, feed) in [
        (
            "inject VBR, pass-through only",
            InjectConfig::default(),
            false,
        ),
        (
            "inject VBR, sync unit per frame",
            InjectConfig::default(),
            true,
        ),
        (
            "inject CBR, sync unit per frame",
            cbr(OnOverflow::DropOldest),
            true,
        ),
    ] {
        time(name, &mut || {
            sink.clear();
            let mut inj = Injector::new(cfg.clone()).unwrap();
            let mut reorder = FrameReorder::new(16);
            for c in made.ts.chunks(65_536) {
                inj.push_with(c, &mut sink, &mut |f: &Frame, u: &mut Units<'_>| {
                    if feed {
                        reorder.push(*f);
                        while let Some(r) = reorder.pop() {
                            let _ = u.insert(InsertUnit::sync(r.pts, vec![0x06; 120]));
                        }
                    }
                })
                .unwrap();
            }
            inj.finish(&mut sink).unwrap();
            sink.len()
        });
    }
    // Pre-timed input (the caller's PcrTimeline is not timed).
    let mut timeline = PcrTimeline::new(None, 10_000_000);
    let mut timed = Vec::new();
    for p in made.ts.chunks(188) {
        timeline.push(p.try_into().unwrap(), &mut |t| timed.push(t));
    }
    timeline.flush(&mut |t| timed.push(t));
    time("inject VBR, push_packet, unit per frame", &mut || {
        sink.clear();
        let mut inj = Injector::new(InjectConfig::default()).unwrap();
        let mut reorder = FrameReorder::new(16);
        for t in &timed {
            inj.push_packet_with(
                &t.packet,
                t.clock,
                &mut sink,
                &mut |f: &Frame, u: &mut Units<'_>| {
                    reorder.push(*f);
                    while let Some(r) = reorder.pop() {
                        let _ = u.insert(InsertUnit::sync(r.pts, vec![0x06; 120]));
                    }
                },
            )
            .unwrap();
        }
        inj.finish(&mut sink).unwrap();
        sink.len()
    });
}

#[test]
fn pmt_version_change_keeps_the_klv_pid_and_service() {
    // Program 1 gains an audio stream (PMT version 3 -> 4) at frame 30.
    let mut ts = Ts::new();
    let v3 = pmt(1, 3, 0x100, &[], &[(H264, 0x100, vec![])]);
    let v4 = pmt(
        1,
        4,
        0x100,
        &[],
        &[(H264, 0x100, vec![]), (0x0F, 0x102, vec![])],
    );
    let mut frames = Vec::new();
    for i in 0..60u64 {
        if i % 3 == 0 {
            ts.section(0, &pat(1, 0, &[(1, 0x1000)]));
            ts.section(0x1000, if i < 30 { &v3 } else { &v4 });
        }
        let clock = 27_000_000 + i * 900_900;
        let opts = harness::common::Opts {
            pcr: Some(clock),
            ..Default::default()
        };
        let mut es = vec![0, 0, 0, 1, 9, 0xF0];
        es.resize(1500, 0x22);
        let pts = clock / 300 + 36_000;
        frames.push(pts);
        send_pes(
            &mut ts,
            0x100,
            opts,
            &harness::common::pes_bytes(0xE0, Some(pts), None, &es),
        );
        if i >= 30 {
            ts.pes(
                0x102,
                0xC0,
                Some(pts),
                &[0xFF, 0xF1, 0x50, 0x80, 0x02, 0x1F, 0xFC],
            );
        }
        for _ in 0..10 {
            ts.null();
        }
    }
    let cfg = InjectConfig::default();
    let r = inject(cfg.clone(), &ts.out, Feed::PerFrame);
    // 0x101 is free in both versions, so it stays; the service id too.
    assert_eq!((r.target.klv_pid, r.target.service_id), (0x101, Some(0)));
    assert!(!r
        .events
        .iter()
        .any(|e| matches!(e, InjectEvent::TargetChanged(t) if t.klv_pid != 0x101)));
    checked(&r, &cfg);
    let versions: Vec<u8> = pmt_sections(&r.out, 0x1000)
        .iter()
        .map(|s| (s[5] >> 1) & 0x1F)
        .collect();
    let first_v5 = versions.iter().position(|&v| v == 5).unwrap();
    assert!(
        versions[..first_v5].iter().all(|&v| v == 4)
            && versions[first_v5..].iter().all(|&v| v == 5)
    );
    let report = mpegts::analyze(&r.out);
    let streams: Vec<u16> = report
        .program(1)
        .unwrap()
        .streams
        .iter()
        .map(|s| s.pid)
        .collect();
    assert_eq!(streams, [0x100, 0x102, 0x101]);
    assert_eq!(read_back(&r.out, 0x101).len(), 60);
    assert_invariant(&ts.out, &r.out, &skip_pids(&r), false);
    let _ = frames;
}

#[test]
fn pcr_on_the_pmt_pid_is_kept() {
    // The program's PCR rides in adaptation-field-only packets on its PMT
    // PID, which the injector owns.
    let mut ts = Ts::new();
    let section = pmt(1, 0, 0x1000, &[], &[(H264, 0x100, vec![])]);
    for i in 0..60u64 {
        if i % 3 == 0 {
            ts.section(0, &pat(1, 0, &[(1, 0x1000)]));
            ts.section(0x1000, &section);
        }
        let clock = 27_000_000 + i * 900_900;
        let pcr = harness::common::Opts {
            pcr: Some(clock),
            no_payload: true,
            ..Default::default()
        };
        ts.packet_with(0x1000, pcr, &[], false);
        let mut es = vec![0, 0, 0, 1, 9, 0xF0];
        es.resize(1500, 0x33);
        ts.pes(0x100, 0xE0, Some(clock / 300 + 36_000), &es);
        for _ in 0..10 {
            ts.null();
        }
    }
    for cfg in [InjectConfig::default(), cbr(OnOverflow::Fail)] {
        let r = inject(cfg.clone(), &ts.out, Feed::PerFrame);
        assert_eq!(r.target.pcr_pid, Some(0x1000));
        checked(&r, &cfg);
        let pcrs = |ts: &[u8]| -> Vec<(usize, u64)> {
            ts.chunks(188)
                .enumerate()
                .filter_map(|(i, p)| {
                    PacketHeader::parse(p.try_into().unwrap())
                        .unwrap()
                        .pcr()
                        .map(|c| (i, c))
                })
                .collect()
        };
        let (a, b) = (pcrs(&ts.out), pcrs(&r.out));
        assert_eq!(
            a.iter().map(|x| x.1).collect::<Vec<_>>(),
            b.iter().map(|x| x.1).collect::<Vec<_>>()
        );
        if matches!(cfg.rate, RateMode::Cbr { .. }) {
            assert_eq!(a, b, "same positions in CBR");
        }
        assert_eq!(r.stats.pcr_only_packets, 60);
        // The last frame comes after the last PCR: finish_with hands it to
        // the callback too.
        assert_eq!(r.inserted.len(), 60);
        assert_eq!(read_back(&r.out, r.target.klv_pid), r.inserted);
    }
}
