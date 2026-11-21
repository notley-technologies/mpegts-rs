//! The injector under random input and random insert schedules (feature
//! `mux`): it must never panic, keep memory within its limits, return only
//! documented errors, and pass every input packet it does not own through
//! unchanged and in order.

use super::common::Rng;
use super::synth::{Prog, Synth};
use super::{generate, Input};
use mpegts::mux::{
    ExistingKlv, Frame, InjectConfig, InjectError, InjectEvent, Injector, InsertUnit, KlvCarriage,
    OnOverflow, ProgramSelect, RateMode, Target, Units,
};
use mpegts::PcrTimeline;
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

pub fn inject_run(data: &[u8]) -> InjectRun {
    let mut input = Input::new(data);
    let cfg = config_from(&mut input);
    let by_packet = input.u8() & 1 != 0;
    let seed = input.u64() | 1;
    let ts = stream_from(&mut input);
    let mut run = InjectRun::default();
    let Ok(mut inj) = Injector::new(cfg.clone()) else {
        return run;
    };

    let mut rng = Rng(seed);
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
        for t in &timed {
            result = inj.push_packet_with(&t.packet, t.clock, &mut out, &mut on_frame);
            if result.is_err() {
                break;
            }
        }
    } else {
        let mut at = 0;
        while at < ts.len() {
            let n = (1 + rng.below(3000)).min(ts.len() - at);
            result = inj.push_with(&ts[at..at + n], &mut out, &mut on_frame);
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
    let finished = result.and_then(|()| inj.finish_with(&mut out, &mut on_frame).map(|_| ()));
    if let Err(e) = &finished {
        check_err(e);
        run.error = true;
        // The error is final.
        assert_eq!(inj.finish(&mut Vec::new()).err().as_ref(), Some(e));
    }
    assert_eq!(out.len() % 188, 0);
    let stats = inj.stats();
    assert!(stats.units_queued as usize <= cfg.max_queued_units);
    assert!(stats.bytes_queued as usize <= cfg.max_queued_bytes);
    assert!(stats.held_packets as usize <= cfg.max_held_packets + 1);
    assert_eq!(stats.packets_out as usize, out.len() / 188);
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
        assert_eq!(len % 188, 0);
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
            let status = if by_packet && ts.len() - at >= 188 {
                let clock = u64::from(ops.u32()) * 300;
                let c = if ops.bool() {
                    &clock as *const u64
                } else {
                    ptr::null()
                };
                let s = mpegts_injector_push_packet(
                    inj,
                    ts[at..].as_ptr(),
                    c,
                    Some(collect),
                    user,
                    Some(on_frame),
                    frame_user,
                );
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
        assert_eq!(out.len() % 188, 0);
        mpegts_injector_free(inj);
    }
}
