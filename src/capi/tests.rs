//! Behaviour of the C ABI, exercised from Rust. Layout and header checks
//! against a real C compiler live in tests/c_abi.rs.

use super::*;

#[path = "../../tests/common/mod.rs"]
#[allow(dead_code)]
mod common;
use common::*;

fn stream() -> Vec<u8> {
    let mut ts = Ts::new();
    ts.section(0, &pat(9, 0, &[(1, 0x1000)]));
    ts.section(
        0x1000,
        &pmt(1, 0, 0x100, &registration(b"KLVA"), &[(H264, 0x100, language(b"eng")), (PRIVATE, 0x102, registration(b"KLVA"))]),
    );
    ts.pcr(0x100, 0);
    for i in 0..3u8 {
        ts.pes(0x100, 0xE0, Some(u64::from(i) * 3003), &[0; 300]);
        let mut klv = KLV_KEY.to_vec();
        klv.extend([1, i]);
        ts.pes(0x102, 0xBD, Some(1000 + u64::from(i)), &klv);
    }
    ts.pcr(0x100, 27_000_000);
    ts.out
}

fn sized<T: Default>() -> T {
    let mut v = T::default();
    // SAFETY: every ABI struct starts with a u32 struct_size.
    unsafe { (ptr::addr_of_mut!(v) as *mut u32).write(mem::size_of::<T>() as u32) };
    v
}

fn new_demuxer(config: Option<&mpegts_config>) -> *mut mpegts_demuxer {
    let mut d = ptr::null_mut();
    let cfg: *const mpegts_config = match config {
        Some(c) => c,
        None => ptr::null(),
    };
    assert_eq!(unsafe { mpegts_demuxer_new(cfg, &mut d) }, MPEGTS_OK);
    assert!(!d.is_null());
    d
}

fn subscribe(d: *mut mpegts_demuxer, select: u32, value: u32, unit: u32) -> u32 {
    let sub = mpegts_subscription { struct_size: 20, select, value, unit, replay: 0 };
    let mut id = 0;
    assert_eq!(unsafe { mpegts_demuxer_subscribe(d, &sub, &mut id) }, MPEGTS_OK);
    id
}

fn drain(d: *mut mpegts_demuxer) -> Vec<(mpegts_output, Vec<u8>)> {
    let mut all = Vec::new();
    loop {
        let mut out: mpegts_output = unsafe { mem::zeroed() };
        out.struct_size = mem::size_of::<mpegts_output>() as u32;
        match unsafe { mpegts_demuxer_next_output(d, &mut out) } {
            MPEGTS_EMPTY => return all,
            MPEGTS_OK => {
                let data = unsafe { slice::from_raw_parts(out.data, out.data_len as usize) }.to_vec();
                all.push((out, data));
            }
            other => panic!("status {other}"),
        }
    }
}

#[test]
fn full_flow() {
    let d = new_demuxer(None);
    let klv = subscribe(d, 3, Codec::KlvAsync.code(), 2);
    let psi = subscribe(d, 5, 0, 3);
    let data = stream();
    unsafe {
        assert_eq!(mpegts_demuxer_push(d, data.as_ptr(), data.len()), MPEGTS_OK);
        assert_eq!(mpegts_demuxer_finish(d), MPEGTS_OK);
    }
    let outs = drain(d);
    let klvs: Vec<_> = outs.iter().filter(|(o, _)| o.subscription == klv).collect();
    assert_eq!(klvs.len(), 3);
    for (i, (o, bytes)) in klvs.iter().enumerate() {
        assert_eq!(o.flags & (OUTPUT_HAS_PTS | OUTPUT_HAS_STREAM_ID), OUTPUT_HAS_PTS | OUTPUT_HAS_STREAM_ID);
        assert_eq!((o.pid, o.stream_id, o.pts), (0x102, 0xBD, 1000 + i as u64));
        assert_eq!(&bytes[o.payload_offset as usize..][..4], &KLV_KEY[..4]);
    }
    let tables: Vec<_> = outs.iter().filter(|(o, _)| o.subscription == psi).map(|(o, _)| (o.pid, o.table_id, o.flags & OUTPUT_CRC_OK)).collect();
    assert_eq!(tables, [(0, 0, OUTPUT_CRC_OK), (0x1000, 2, OUTPUT_CRC_OK)]);

    let mut stats: mpegts_output_stats = sized();
    unsafe { assert_eq!(mpegts_demuxer_output_stats(d, &mut stats), MPEGTS_OK) };
    assert_eq!((stats.subscriptions, stats.queued, stats.pending), (2, 5, 0));

    let mut report = ptr::null_mut();
    unsafe { assert_eq!(mpegts_demuxer_report(d, &mut report), MPEGTS_OK) };
    let mut summary: mpegts_report_summary = sized();
    unsafe { assert_eq!(mpegts_report_summary_get(report, &mut summary), MPEGTS_OK) };
    assert_eq!((summary.transport_stream_id, summary.program_count, summary.packet_size), (9, 1, 188));
    assert_eq!(summary.flags & 0x3F, 0x01 | 0x02 | 0x08 | 0x10 | 0x20);
    assert_eq!(summary.duration_ns, 1_000_000_000);

    let mut prog: mpegts_program_info = sized();
    unsafe { assert_eq!(mpegts_report_program(report, 0, &mut prog), MPEGTS_OK) };
    assert_eq!((prog.number, prog.pmt_pid, prog.pcr_pid, prog.stream_count), (1, 0x1000, 0x100, 2));
    assert_eq!(&prog.registration, b"KLVA");

    let mut s: mpegts_stream_info = sized();
    unsafe { assert_eq!(mpegts_report_stream(report, 0, 0, &mut s), MPEGTS_OK) };
    assert_eq!((s.pid, s.codec, s.kind, s.program), (0x100, Codec::H264.code(), 1, 1));
    assert_eq!(s.language.map(|c| c as u8), *b"eng\0");
    assert_ne!(s.flags & 0x001, 0);
    unsafe { assert_eq!(mpegts_report_stream(report, 0, 1, &mut s), MPEGTS_OK) };
    assert_eq!((s.pid, s.codec, s.kind), (0x102, Codec::KlvAsync.code(), 3));

    let (mut tag, mut p, mut len) = (0u8, ptr::null(), 0u64);
    unsafe { assert_eq!(mpegts_report_stream_descriptor(report, 0, 1, 0, &mut tag, &mut p, &mut len), MPEGTS_OK) };
    assert_eq!((tag, unsafe { slice::from_raw_parts(p, len as usize) }), (0x05, &b"KLVA"[..]));
    unsafe { assert_eq!(mpegts_report_program_descriptor(report, 0, 0, &mut tag, &mut p, &mut len), MPEGTS_OK) };
    assert_eq!(tag, 0x05);

    let mut pid: mpegts_pid_info = sized();
    unsafe { assert_eq!(mpegts_report_pid(report, 0, &mut pid), MPEGTS_OK) };
    assert_eq!((pid.pid, pid.packets), (0, 1));

    unsafe {
        assert_eq!(mpegts_report_program(report, 1, &mut prog), MPEGTS_ERR_RANGE);
        assert_eq!(mpegts_report_stream(report, 0, 2, &mut s), MPEGTS_ERR_RANGE);
        assert_eq!(mpegts_report_stream_descriptor(report, 0, 1, 1, &mut tag, &mut p, &mut len), MPEGTS_ERR_RANGE);
        let (mut sys, mut ca_pid) = (0u16, 0u16);
        assert_eq!(mpegts_report_ca_system(report, 0, &mut sys, &mut ca_pid), MPEGTS_ERR_RANGE);
        mpegts_report_free(report);
        assert_eq!(mpegts_demuxer_unsubscribe(d, klv), MPEGTS_OK);
        assert_eq!(mpegts_demuxer_unsubscribe(d, klv), MPEGTS_ERR_NOT_FOUND);
        mpegts_demuxer_free(d);
    }
}

#[test]
fn null_pointers_are_rejected() {
    unsafe {
        let mut d = ptr::null_mut();
        assert_eq!(mpegts_demuxer_new(ptr::null(), ptr::null_mut()), MPEGTS_ERR_NULL);
        assert_eq!(mpegts_demuxer_push(ptr::null_mut(), ptr::null(), 0), MPEGTS_ERR_NULL);
        assert_eq!(mpegts_demuxer_finish(ptr::null_mut()), MPEGTS_ERR_NULL);
        assert_eq!(mpegts_demuxer_revision(ptr::null(), ptr::null_mut()), MPEGTS_ERR_NULL);
        assert_eq!(mpegts_demuxer_report(ptr::null(), &mut ptr::null_mut()), MPEGTS_ERR_NULL);
        assert_eq!(mpegts_config_default(ptr::null_mut()), MPEGTS_ERR_NULL);
        assert_eq!(mpegts_report_summary_get(ptr::null(), ptr::null_mut()), MPEGTS_ERR_NULL);
        mpegts_demuxer_free(ptr::null_mut());
        mpegts_report_free(ptr::null_mut());

        assert_eq!(mpegts_demuxer_new(ptr::null(), &mut d), MPEGTS_OK);
        assert_eq!(mpegts_demuxer_push(d, ptr::null(), 10), MPEGTS_ERR_NULL);
        assert_eq!(mpegts_demuxer_push(d, ptr::null(), 0), MPEGTS_OK);
        assert_eq!(mpegts_demuxer_next_output(d, ptr::null_mut()), MPEGTS_ERR_NULL);
        assert_eq!(mpegts_demuxer_subscribe(d, ptr::null(), &mut 0), MPEGTS_ERR_NULL);
        let sub = mpegts_subscription { struct_size: 20, select: 6, value: 0, unit: 1, replay: 0 };
        assert_eq!(mpegts_demuxer_subscribe(d, &sub, ptr::null_mut()), MPEGTS_ERR_NULL);
        assert_eq!(mpegts_demuxer_history_get(d, 0, ptr::null_mut(), ptr::null_mut()), MPEGTS_ERR_NULL);
        mpegts_demuxer_free(d);
    }
}

#[test]
fn invalid_arguments_are_rejected() {
    unsafe {
        let d = new_demuxer(None);
        let mut id = 0;
        for (select, value, unit) in [(1, 0x2000, 1), (3, 0xDEAD, 2), (4, 99, 2), (9, 0, 1), (6, 0, 0), (6, 0, 4), (2, 0x10000, 1)] {
            let sub = mpegts_subscription { struct_size: 20, select, value, unit, replay: 0 };
            assert_eq!(mpegts_demuxer_subscribe(d, &sub, &mut id), MPEGTS_ERR_INVALID, "{select} {value} {unit}");
        }
        let bad_size = mpegts_subscription { struct_size: 2, select: 6, value: 0, unit: 1, replay: 0 };
        assert_eq!(mpegts_demuxer_subscribe(d, &bad_size, &mut id), MPEGTS_ERR_INVALID);
        mpegts_demuxer_free(d);

        let mut config = mpegts_config::from(&Config::default());
        config.sync_confirmations = 0;
        let mut d = ptr::null_mut();
        assert_eq!(mpegts_demuxer_new(&config, &mut d), MPEGTS_ERR_INVALID);
        assert!(d.is_null());
        config.sync_confirmations = 2;
        config.overflow = 7;
        assert_eq!(mpegts_demuxer_new(&config, &mut d), MPEGTS_ERR_INVALID);
    }
}

#[test]
fn struct_size_versioning() {
    unsafe {
        // An older caller that only knows the first four config fields.
        let mut old = mpegts_config::from(&Config::default());
        old.struct_size = 16;
        old.sync_confirmations = 3;
        old.history_packets = 999_999_999; // beyond struct_size: must be ignored
        let d = new_demuxer(Some(&old));
        assert_eq!((*d).demux.config().sync_confirmations, 3);
        assert_eq!((*d).demux.config().history_packets, 0);

        // A newer caller with a bigger struct: the extra tail is untouched.
        #[repr(C)]
        struct Newer {
            info: mpegts_output_stats,
            extra: u64,
        }
        let mut n = Newer { info: sized(), extra: 0xABCD };
        n.info.struct_size = mem::size_of::<Newer>() as u32;
        assert_eq!(mpegts_demuxer_output_stats(d, &mut n.info), MPEGTS_OK);
        assert_eq!(n.info.struct_size as usize, mem::size_of::<mpegts_output_stats>());
        assert_eq!(n.extra, 0xABCD);

        // An older caller with a smaller output struct: only that much written.
        let mut buf = [0xEEu8; 64];
        (buf.as_mut_ptr() as *mut u32).write(8);
        let p = buf.as_mut_ptr() as *mut mpegts_output_stats;
        assert_eq!(mpegts_demuxer_output_stats(d, p), MPEGTS_OK);
        assert_eq!((p as *const u32).read(), 8);
        assert!(buf[8..].iter().all(|&b| b == 0xEE));

        let mut cfg: mpegts_config = mem::zeroed();
        cfg.struct_size = mem::size_of::<mpegts_config>() as u32;
        assert_eq!(mpegts_config_default(&mut cfg), MPEGTS_OK);
        assert_eq!((cfg.sync_confirmations, cfg.max_streams, cfg.probe_pes), (2, 4096, 4));
        mpegts_demuxer_free(d);
    }
}

#[test]
fn poisoned_demuxer_refuses_work() {
    unsafe {
        let d = new_demuxer(None);
        (*d).poisoned = true;
        assert_eq!(mpegts_demuxer_push(d, [0u8].as_ptr(), 1), MPEGTS_ERR_POISONED);
        assert_eq!(mpegts_demuxer_finish(d), MPEGTS_ERR_POISONED);
        let mut out: mpegts_output = mem::zeroed();
        out.struct_size = mem::size_of::<mpegts_output>() as u32;
        assert_eq!(mpegts_demuxer_next_output(d, &mut out), MPEGTS_ERR_POISONED);
        mpegts_demuxer_free(d);
    }
    // A panic inside the guard poisons the handle instead of unwinding.
    let mut h = mpegts_demuxer { demux: Demuxer::new(), current: None, poisoned: false };
    let status = guard_mut(&mut h, |_| panic!("boom"));
    assert_eq!((status, h.poisoned), (MPEGTS_ERR_PANIC, true));
}

#[test]
fn history_and_replay() {
    unsafe {
        let mut config = mpegts_config::from(&Config::default());
        config.history_packets = 1000;
        let d = new_demuxer(Some(&config));
        let data = stream();
        mpegts_demuxer_push(d, data.as_ptr(), data.len());
        let mut n = 0;
        assert_eq!(mpegts_demuxer_history_len(d, &mut n), MPEGTS_OK);
        assert_eq!(n as usize, data.len() / 188);
        let (mut packet, mut p) = (0u64, ptr::null());
        assert_eq!(mpegts_demuxer_history_get(d, 0, &mut packet, &mut p), MPEGTS_OK);
        assert_eq!((packet, slice::from_raw_parts(p, 188)), (1, &data[..188]));
        assert_eq!(mpegts_demuxer_history_get(d, n, &mut packet, &mut p), MPEGTS_ERR_RANGE);
        let sub = mpegts_subscription { struct_size: 20, select: 3, value: Codec::KlvAsync.code(), unit: 2, replay: 1 };
        let mut id = 0;
        assert_eq!(mpegts_demuxer_subscribe(d, &sub, &mut id), MPEGTS_OK);
        assert_eq!(drain(d).len(), 3, "all KLV from before the subscription");
        mpegts_demuxer_free(d);
    }
}

#[test]
fn names_and_versions() {
    use std::ffi::CStr;
    let s = |p: *const c_char| unsafe { CStr::from_ptr(p) }.to_str().unwrap();
    assert_eq!(mpegts_abi_version(), 1);
    assert_eq!(s(mpegts_version()), env!("CARGO_PKG_VERSION"));
    assert_eq!(s(mpegts_codec_name(Codec::KlvSync.code())), "KLV (sync)");
    assert!(mpegts_codec_name(0xFFFF).is_null());
    assert_eq!(mpegts_codec_kind(Codec::H265.code()), 1);
    assert_eq!(s(mpegts_kind_name(3)), "metadata");
    assert!(mpegts_kind_name(77).is_null());
    assert_eq!(s(mpegts_status_string(MPEGTS_ERR_RANGE)), "index out of range");
    assert_eq!(s(mpegts_status_string(12345)), "unknown status");
    for c in Codec::ALL {
        assert_eq!(Codec::from_code(c.code()), Some(c));
        assert_eq!(c.code() >> 8, c.kind().code(), "{c:?}: code range matches kind");
    }
}

#[test]
fn analyze_in_one_call() {
    let data = stream();
    unsafe {
        let mut r = ptr::null_mut();
        assert_eq!(mpegts_analyze(data.as_ptr(), data.len(), &mut r), MPEGTS_OK);
        assert_eq!((*r).report, crate::analyze(&data));
        mpegts_report_free(r);

        assert_eq!(mpegts_analyze(ptr::null(), 0, &mut r), MPEGTS_OK, "empty input is fine");
        let mut summary: mpegts_report_summary = sized();
        assert_eq!(mpegts_report_summary_get(r, &mut summary), MPEGTS_OK);
        assert_eq!((summary.packets, summary.program_count), (0, 0));
        mpegts_report_free(r);

        assert_eq!(mpegts_analyze(ptr::null(), 1, &mut r), MPEGTS_ERR_NULL);
        assert!(r.is_null(), "out is cleared on error");
        assert_eq!(mpegts_analyze(data.as_ptr(), data.len(), ptr::null_mut()), MPEGTS_ERR_NULL);
        assert_eq!(mpegts_analyze(data.as_ptr(), usize::MAX, &mut r), MPEGTS_ERR_INVALID);
    }
}

#[test]
fn config_check_explains_rejections() {
    use std::ffi::CStr;
    let check = |c: &mpegts_config, len: usize| {
        let mut buf: [c_char; 128] = [0x55; 128];
        let status = unsafe { mpegts_config_check(c, buf.as_mut_ptr(), len) };
        let msg = if len == 0 {
            assert!(buf.iter().all(|&b| b == 0x55), "nothing written for a zero-length buffer");
            String::new()
        } else {
            unsafe { CStr::from_ptr(buf.as_ptr()) }.to_str().unwrap().to_string()
        };
        (status, msg)
    };
    let good = mpegts_config::from(&Config::default());
    assert_eq!(check(&good, 128), (MPEGTS_OK, String::new()));

    let mut c = good;
    c.max_pes_bytes = 10;
    let (status, msg) = check(&c, 128);
    assert_eq!(status, MPEGTS_ERR_INVALID);
    assert!(msg.contains("max_pes_bytes"), "{msg}");
    // Cut to fit, still NUL-terminated.
    assert_eq!(check(&c, 8), (MPEGTS_ERR_INVALID, msg[..7].to_string()));
    assert_eq!(check(&c, 1), (MPEGTS_ERR_INVALID, String::new()));
    assert_eq!(check(&c, 0).0, MPEGTS_ERR_INVALID);

    c = good;
    c.overflow = 9;
    assert!(check(&c, 128).1.contains("overflow"));
    c = good;
    c.struct_size = 2;
    assert!(check(&c, 128).1.contains("struct_size"));
    unsafe {
        assert_eq!(mpegts_config_check(&good, ptr::null_mut(), 64), MPEGTS_OK, "message is optional");
        assert_eq!(mpegts_config_check(ptr::null(), ptr::null_mut(), 0), MPEGTS_ERR_NULL);
    }
    // What check accepts, demuxer_new accepts, and the other way round.
    for sync in 0..10 {
        c = good;
        c.sync_confirmations = sync;
        let mut d = ptr::null_mut();
        let created = unsafe { mpegts_demuxer_new(&c, &mut d) };
        assert_eq!(created, check(&c, 128).0);
        unsafe { mpegts_demuxer_free(d) };
    }
}
