//! The injector under random input and random insert schedules (feature
//! `mux`): it must never panic, keep memory within its limits, return only
//! documented errors, and pass every input packet it does not own through
//! unchanged and in order.

use super::common::Rng;
use super::es::{EsConfig, Video};
use super::synth::{Prog, Synth};
use super::{generate, Input};
use mpegts::mux::{
    ExistingKlv, Frame, InjectConfig, InjectError, InjectEvent, Injector, InsertUnit, KlvCarriage,
    OnOverflow, ProgramSelect, RateMode, Record, RecordFormat, Target, Units, VideoSei,
};
use mpegts::{ClockMode, PcrTimeline};
use std::collections::HashSet;

/// What one run did, so a test can check that random inputs reach the
/// interesting paths.
#[derive(Debug, Default, Clone, Copy)]
pub struct InjectRun {
    /// A target program was found.
    pub target: bool,
    /// Units written.
    pub units: u64,
    /// The run ended in an error.
    pub error: bool,
}

pub fn inject(data: &[u8]) {
    inject_run(data);
}

fn config_from(input: &mut Input<'_>) -> InjectConfig {
    let b = input.u8();
    let mut c = InjectConfig::default();
    c.carriage = if b & 1 == 0 {
        KlvCarriage::Sync
    } else {
        KlvCarriage::Async
    };
    c.existing_klv =
        [ExistingKlv::Keep, ExistingKlv::Replace, ExistingKlv::Fail][usize::from(b >> 1) % 3];
    c.rate = match (b >> 3) % 4 {
        0 => RateMode::Cbr {
            on_overflow: OnOverflow::DropOldest,
        },
        1 => RateMode::Cbr {
            on_overflow: OnOverflow::Fail,
        },
        _ => RateMode::Vbr,
    };
    if b & 0x20 != 0 {
        c.program = ProgramSelect::Number(u16::from(input.u8() % 4).max(1));
    }
    c.align_to_frames = b & 0x40 == 0;
    if b & 0x80 != 0 {
        // Tight limits, so the limit paths run.
        c.max_queued_units = 1 + usize::from(input.u8() % 8);
        c.max_queued_bytes = 1 + usize::from(input.u16()) * 4;
        c.max_held_packets = 1 + usize::from(input.u16() % 2000);
        c.psi_interval_ms = 1 + u32::from(input.u8() % 200);
    }
    if input.u8() < 32 {
        c.pid = Some(0x0010 + input.u16() % 0x1FEF);
    }
    c
}

/// A synthetic stream, sometimes, so valid structure is common; otherwise
/// the generator's random tables and PES.
fn stream_from(input: &mut Input<'_>) -> Vec<u8> {
    let b = input.u8();
    if b % 4 != 0 {
        return generate(input);
    }
    let mut s = Synth {
        frames: 4 + u64::from(input.u8() % 40),
        b_frames: b & 4 != 0,
        ..Synth::default()
    };
    if b & 8 != 0 {
        s.programs.push(Prog::new(2, 0x1001, 0x200));
    }
    if b & 16 != 0 {
        s.programs[0] = s.programs[0].clone().with(super::synth::Existing::SyncKlv {
            pid: 0x101,
            service: input.u8() % 3,
        });
    }
    if b & 32 != 0 {
        s.discontinuity_at = Some(u64::from(input.u8()) % s.frames);
        s.jump = i64::from(input.u32() as i32) * 300;
    }
    if b & 64 != 0 {
        s.pmt_pid_change = Some((u64::from(input.u8()) % s.frames, 0x1100));
    }
    s.first_dts = input.u64() % (1 << 33);
    if b & 128 != 0 {
        // Synthetic H.264, H.265 or MPEG-2 video with SEI user data.
        let e = input.u8();
        let mut cfg = EsConfig::new([Video::H264, Video::H265, Video::Mpeg2][usize::from(e % 3)]);
        cfg.other = e & 4 != 0;
        cfg.misp = e & 8 != 0;
        cfg.misp_second = e & 16 != 0;
        cfg.prefix_nal = e & 32 != 0;
        s.video_unbounded = e & 64 != 0;
        if e & 128 != 0 {
            // Last packets without stuffing.
            s.pes_len = Some(176 + 184 * 3 + 184);
        }
        s.es = Some(cfg);
    }
    let mut ts = s.make().ts;
    // A few byte edits.
    for _ in 0..input.u8() % 4 {
        if ts.is_empty() {
            break;
        }
        let i = input.u32() as usize % ts.len();
        ts[i] ^= input.u8();
    }
    ts
}

/// Video user data, clock and record options, from a byte read after the
/// stream (so older inputs keep their meaning).
fn video_options(cfg: &mut InjectConfig, input: &mut Input<'_>) {
    let v = input.u8();
    cfg.video_sei = [VideoSei::Off, VideoSei::Scan, VideoSei::Write][usize::from(v % 3)];
    if v & 4 != 0 {
        cfg.clock = ClockMode::Extrapolate;
    }
    if v & 8 != 0 {
        cfg.record = RecordFormat::M2ts;
    }
    if v & 16 != 0 {
        cfg.video_sei_max_prefix = 16 + usize::from(input.u8());
    }
}

/// The stream as M2TS records with increasing arrival time stamps.
fn to_m2ts(ts: &[u8], rng: &mut Rng) -> Vec<u8> {
    let mut out = Vec::with_capacity(ts.len() / 188 * 192);
    let mut ats = rng.next() as u32;
    for p in ts.chunks_exact(188) {
        ats = ats.wrapping_add(1 + rng.below(20_000) as u32);
        let cpi = (rng.next() as u32) & 0xC000_0000;
        out.extend_from_slice(&(cpi | (ats & 0x3FFF_FFFF)).to_be_bytes());
        out.extend_from_slice(p);
    }
    out
}

pub fn inject_run(data: &[u8]) -> InjectRun {
    let mut input = Input::new(data);
    let cfg = config_from(&mut input);
    let by_packet = input.u8() & 1 != 0;
    let seed = input.u64() | 1;
    let ts = stream_from(&mut input);
    let mut cfg = cfg;
    video_options(&mut cfg, &mut input);
    let m2ts = cfg.record == RecordFormat::M2ts;
    let mut run = InjectRun::default();
    let Ok(mut inj) = Injector::new(cfg.clone()) else {
        return run;
    };

    let mut rng = Rng(seed);
    let records = if m2ts { to_m2ts(&ts, &mut rng) } else { Vec::new() };
    let unit = if m2ts { 192 } else { 188 };
    let mut out = Vec::new();
    let mut targets: Vec<Target> = Vec::new();
    let check_err = |e: &InjectError| {
        assert!(
            !matches!(e, InjectError::Mux(_)),
            "internal write error: {e:?}"
        );
        let _ = e.to_string();
    };
    let mut on_frame = {
        let mut rng = Rng(seed ^ 0x9E37);
        move |f: &Frame, u: &mut Units<'_>| {
            for _ in 0..rng.below(3) {
                let len = match rng.below(16) {
                    0 => 60_000 + rng.below(20_000),
                    1 => 0,
                    _ => 1 + rng.below(600),
                };
                let data = vec![0x06; len];
                let unit = if u.carriage() == KlvCarriage::Async || rng.below(16) == 0 {
                    InsertUnit::asynchronous(data)
                } else {
                    let d = rng.below(180_000) as u64;
                    InsertUnit::sync((f.pts + d).wrapping_sub(90_000) % (1 << 33), data)
                };
                if let Err(e) = u.insert(unit) {
                    assert!(
                        matches!(
                            e,
                            InjectError::InvalidUnit(_)
                                | InjectError::OutOfOrder { .. }
                                | InjectError::QueueFull { .. }
                        ),
                        "{e:?}"
                    );
                }
            }
            let _ = (u.clock(), u.target(), u.queued());
            // Video user data: read, and sometimes written.
            for m in u.user_data() {
                assert!(m.position.start <= m.position.header && m.position.header <= m.position.end);
            }
            if u.video_prefix().is_some() && rng.below(3) != 0 {
                let uuid = [[0x4D; 16], [0xA8; 16], *b"other-user-data!"][rng.below(3)];
                let len = match rng.below(16) {
                    0 => 2000 + rng.below(70_000),
                    _ => rng.below(64),
                };
                let payload: Vec<u8> = (0..len).map(|_| [0, 0, 3, 1, 0xFF][rng.below(5)]).collect();
                let replace = [[0x4D; 16], *b"MISPmicrosectime"];
                match u.write_user_data(uuid, &payload, &replace[..rng.below(3)]) {
                    Ok(()) | Err(InjectError::UserData(_)) => {}
                    Err(e) => panic!("{e:?}"),
                }
            }
        }
    };
    let mut result: Result<(), InjectError> = Ok(());
    if by_packet {
        let mut timeline = PcrTimeline::new(None, 1_000_000).with_max_held(64);
        let mut timed = Vec::new();
        for p in ts.chunks_exact(188) {
            timeline.push(p.try_into().unwrap(), &mut |t| timed.push(t));
        }
        timeline.flush(&mut |t| timed.push(t));
        for (i, t) in timed.iter().enumerate() {
            result = if m2ts {
                let r: &Record = records[i * 192..(i + 1) * 192].try_into().unwrap();
                inj.push_record_with(r, t.clock, &mut out, &mut on_frame)
            } else {
                inj.push_packet_with(&t.packet, t.clock, &mut out, &mut on_frame)
            };
            if result.is_err() {
                break;
            }
        }
    } else {
        let input: &[u8] = if m2ts { &records } else { &ts };
        let mut at = 0;
        while at < input.len() {
            let n = (1 + rng.below(3000)).min(input.len() - at);
            result = inj.push_with(&input[at..at + n], &mut out, &mut on_frame);
            if result.is_err() {
                break;
            }
            // Units from outside the callback too.
            if rng.below(4) == 0 {
                let pts = inj
                    .clock()
                    .map_or(0, |c| (c / 300 + rng.below(90_000) as u64) % (1 << 33));
                let unit = match cfg.carriage {
                    KlvCarriage::Sync => InsertUnit::sync(pts, vec![1; 1 + rng.below(300)]),
                    _ => InsertUnit::asynchronous(vec![1; 1 + rng.below(300)]),
                };
                let _ = inj.insert(unit);
            }
            at += n;
        }
    }
    let _ = &ts;
    let finished = result.and_then(|()| inj.finish_with(&mut out, &mut on_frame).map(|_| ()));
    if let Err(e) = &finished {
        check_err(e);
        run.error = true;
        // The error is final.
        assert_eq!(inj.finish(&mut Vec::new()).err().as_ref(), Some(e));
    }
    assert_eq!(out.len() % unit, 0);
    let stats = inj.stats();
    assert!(stats.units_queued as usize <= cfg.max_queued_units);
    assert!(stats.bytes_queued as usize <= cfg.max_queued_bytes);
    assert!(stats.held_packets as usize <= cfg.max_held_packets + 1);
    assert_eq!(stats.packets_out as usize, out.len() / unit);
    let out: Vec<u8> = if m2ts {
        out.chunks_exact(192).flat_map(|r| r[4..].to_vec()).collect()
    } else {
        out
    };
    while let Some(e) = inj.pop_event() {
        if let InjectEvent::TargetFound(t) | InjectEvent::TargetChanged(t) = e {
            targets.push(t);
        }
    }
    targets.extend(inj.target().cloned());
    run.target = !targets.is_empty();
    run.units = stats.units_inserted;
    if finished.is_err() || stats.events_dropped > 0 || !by_packet {
        return run;
    }
    // CBR keeps the packet count.
    if matches!(cfg.rate, RateMode::Cbr { .. }) {
        assert_eq!(stats.packets_out, stats.packets_in);
    }
    // Pass-through: every packet of a PID the injector never owned comes
    // out unchanged and in order.
    let mut skip: HashSet<u16> = [0x0000, 0x1FFF].into_iter().collect();
    for t in &targets {
        skip.insert(t.pmt_pid);
        skip.insert(t.klv_pid);
        skip.extend(&t.stripped);
        if cfg.video_sei == VideoSei::Write {
            // Written video changes; its own checks are in tests/video_sei.rs.
            skip.extend(t.video_pid);
        }
    }
    let keep = |p: &&[u8]| !skip.contains(&u16::from_be_bytes([p[1] & 0x1F, p[2]]));
    let a: Vec<&[u8]> = ts.chunks_exact(188).filter(keep).collect();
    let b: Vec<&[u8]> = out.chunks_exact(188).filter(keep).collect();
    assert!(
        a == b,
        "pass-through packets changed ({} in, {} out)",
        a.len(),
        b.len()
    );
    run
}

// ---------------------------------------------------------------- C API

#[cfg(feature = "capi")]
pub use c::capi_inject_session;

#[cfg(feature = "capi")]
mod c {
    use super::super::{expect, generate, struct_size, Input};
    use mpegts::capi::*;
    use std::os::raw::{c_char, c_void};
    use std::{mem, ptr, slice};

    unsafe extern "C" fn collect(user: *mut c_void, packets: *const u8, len: usize) {
        assert!(len % 188 == 0 || len % 192 == 0);
        let out = &mut *(user as *mut Vec<u8>);
        out.extend_from_slice(slice::from_raw_parts(packets, len));
    }

    unsafe extern "C" fn on_frame(
        user: *mut c_void,
        frame: *const mpegts_inject_frame,
        units: *mut mpegts_inject_units,
    ) {
        let n = &mut *(user as *mut u32);
        *n = n.wrapping_mul(1_103_515_245).wrapping_add(12_345);
        let data = vec![0x06u8; 1 + (*n as usize >> 16) % 400];
        let mut unit: mpegts_inject_unit = mem::zeroed();
        unit.struct_size =
            mem::size_of::<mpegts_inject_unit>() as u32 - if *n & 0x100 != 0 { 8 } else { 0 };
        unit.flags = if *n & 1 == 0 {
            MPEGTS_INJECT_UNIT_HAS_PTS
        } else {
            0
        };
        unit.pts = (*frame).pts.wrapping_add(u64::from(*n >> 20));
        unit.data = data.as_ptr();
        unit.data_len = data.len() as u64;
        let status = mpegts_inject_units_insert(units, &unit);
        expect(
            status,
            &[MPEGTS_OK, MPEGTS_ERR_INVALID, MPEGTS_ERR_FULL],
            "units_insert",
        );
        let mut clock = 0u64;
        expect(
            mpegts_inject_units_clock(units, &mut clock),
            &[MPEGTS_OK, MPEGTS_EMPTY],
            "units_clock",
        );
        let mut t: mpegts_inject_target = mem::zeroed();
        t.struct_size = mem::size_of::<mpegts_inject_target>() as u32;
        expect(
            mpegts_inject_units_target(units, &mut t),
            &[MPEGTS_OK, MPEGTS_EMPTY],
            "units_target",
        );
        // Video user data, read and written.
        let mut uuid = [0u8; 16];
        let mut data: *const u8 = ptr::null();
        let mut len = 0u64;
        let index = (*n >> 8) % ((*frame).user_data_count + 1);
        expect(
            mpegts_inject_units_user_data(units, index, uuid.as_mut_ptr(), &mut data, &mut len),
            &[MPEGTS_OK, MPEGTS_ERR_RANGE],
            "units_user_data",
        );
        let payload = [0u8, 0, 0, 3, 1];
        let replace = [0x4Du8; 32];
        expect(
            mpegts_inject_units_write_user_data(
                units,
                uuid.as_ptr(),
                payload.as_ptr(),
                (*n as usize >> 12) % 6,
                replace.as_ptr(),
                (*n as usize >> 4) % 3,
            ),
            &[MPEGTS_OK, MPEGTS_ERR_INVALID],
            "units_write_user_data",
        );
    }

    /// Random calls on an injector, with odd configurations and struct
    /// sizes; only documented statuses may come back.
    pub unsafe fn capi_inject_session(ops: &mut Input<'_>) {
        let mut cfg: mpegts_inject_config = mem::zeroed();
        cfg.struct_size = mem::size_of::<mpegts_inject_config>() as u32;
        expect(
            mpegts_inject_config_default(&mut cfg),
            &[MPEGTS_OK],
            "config_default",
        );
        match ops.u8() % 8 {
            0 => cfg.flags = ops.u32(),
            1 => cfg.rate = u32::from(ops.u8() % 4),
            2 => cfg.existing_klv = u32::from(ops.u8() % 4),
            3 => cfg.carriage = u32::from(ops.u8() % 3),
            4 => cfg.struct_size = struct_size::<mpegts_inject_config>(ops),
            5 => cfg.max_held_packets = u64::from(ops.u16()),
            6 => {
                cfg.video_sei = u32::from(ops.u8() % 4);
                cfg.clock = u32::from(ops.u8() % 3);
                cfg.video_sei_max_prefix = u32::from(ops.u16());
            }
            7 => cfg.record = u32::from(ops.u8() % 3),
            _ => {}
        }
        let mut msg = [0 as c_char; 64];
        expect(
            mpegts_inject_config_check(&cfg, msg.as_mut_ptr(), msg.len()),
            &[MPEGTS_OK, MPEGTS_ERR_INVALID],
            "check",
        );
        let mut inj = ptr::null_mut();
        let status = mpegts_injector_new(&cfg, &mut inj);
        expect(status, &[MPEGTS_OK, MPEGTS_ERR_INVALID], "injector_new");
        if status != MPEGTS_OK {
            assert!(inj.is_null());
            return;
        }
        let ts = generate(ops);
        let mut out: Vec<u8> = Vec::new();
        let mut state = 7u32;
        let user = &mut out as *mut Vec<u8> as *mut c_void;
        let frame_user = &mut state as *mut u32 as *mut c_void;
        const PUSH: &[i32] = &[
            MPEGTS_OK,
            MPEGTS_ERR_INVALID,
            MPEGTS_ERR_STREAM,
            MPEGTS_ERR_NULL,
        ];
        let mut at = 0;
        let by_packet = ops.bool();
        while at < ts.len() {
            let n = (1 + usize::from(ops.u16() % 2000)).min(ts.len() - at);
            let status = if by_packet && ts.len() - at >= 192 {
                let clock = u64::from(ops.u32()) * 300;
                let c = if ops.bool() {
                    &clock as *const u64
                } else {
                    ptr::null()
                };
                let s = if cfg.record == MPEGTS_RECORD_M2TS {
                    mpegts_injector_push_record(
                        inj,
                        ts[at..].as_ptr(),
                        c,
                        Some(collect),
                        user,
                        Some(on_frame),
                        frame_user,
                    )
                } else {
                    mpegts_injector_push_packet(
                        inj,
                        ts[at..].as_ptr(),
                        c,
                        Some(collect),
                        user,
                        Some(on_frame),
                        frame_user,
                    )
                };
                at += 188;
                s
            } else {
                let s = mpegts_injector_push(
                    inj,
                    ts[at..].as_ptr(),
                    n,
                    Some(collect),
                    user,
                    Some(on_frame),
                    frame_user,
                );
                at += n;
                s
            };
            expect(status, PUSH, "push");
            match ops.u8() % 6 {
                0 => {
                    let data = [1u8, 2, 3];
                    let mut unit: mpegts_inject_unit = mem::zeroed();
                    unit.struct_size = struct_size::<mpegts_inject_unit>(ops);
                    unit.flags = ops.u32() & 3;
                    unit.pts = ops.u64();
                    unit.data = if ops.bool() {
                        data.as_ptr()
                    } else {
                        ptr::null()
                    };
                    unit.data_len = u64::from(ops.u8() % 4);
                    let status = mpegts_injector_insert(inj, &unit);
                    expect(
                        status,
                        &[
                            MPEGTS_OK,
                            MPEGTS_ERR_INVALID,
                            MPEGTS_ERR_FULL,
                            MPEGTS_ERR_NULL,
                            MPEGTS_ERR_STREAM,
                        ],
                        "insert",
                    );
                }
                1 => {
                    let mut ev: mpegts_inject_event = mem::zeroed();
                    ev.struct_size = struct_size::<mpegts_inject_event>(ops);
                    let status = mpegts_injector_pop_event(inj, &mut ev);
                    expect(
                        status,
                        &[MPEGTS_OK, MPEGTS_EMPTY, MPEGTS_ERR_INVALID],
                        "pop_event",
                    );
                }
                2 => {
                    let mut s: mpegts_inject_stats = mem::zeroed();
                    s.struct_size = struct_size::<mpegts_inject_stats>(ops);
                    expect(
                        mpegts_injector_stats(inj, &mut s),
                        &[MPEGTS_OK, MPEGTS_ERR_INVALID],
                        "stats",
                    );
                }
                3 => {
                    let mut t: mpegts_inject_target = mem::zeroed();
                    t.struct_size = struct_size::<mpegts_inject_target>(ops);
                    expect(
                        mpegts_injector_target(inj, &mut t),
                        &[MPEGTS_OK, MPEGTS_EMPTY, MPEGTS_ERR_INVALID],
                        "target",
                    );
                    let mut clock = 0u64;
                    expect(
                        mpegts_injector_clock(inj, &mut clock),
                        &[MPEGTS_OK, MPEGTS_EMPTY],
                        "clock",
                    );
                }
                4 => {
                    expect(
                        mpegts_injector_error(inj, msg.as_mut_ptr(), msg.len()),
                        &[MPEGTS_OK],
                        "error",
                    );
                }
                _ => {}
            }
        }
        let status = mpegts_injector_finish(
            inj,
            Some(collect),
            user,
            Some(on_frame),
            frame_user,
            ptr::null_mut(),
        );
        expect(
            status,
            &[MPEGTS_OK, MPEGTS_ERR_INVALID, MPEGTS_ERR_STREAM],
            "finish",
        );
        assert!(out.len() % 188 == 0 || out.len() % 192 == 0);
        mpegts_injector_free(inj);
    }
}
