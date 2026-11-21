//! The write side of the C ABI as a C compiler sees it: header constants,
//! struct layout, and a C program that writes a stream which the demuxer
//! then reads back.
use mpegts::capi::*;
use mpegts::{Codec, Demuxer, PesHeader, Select, Subscription, Unit};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn header_defines() -> HashMap<String, i64> {
    let text = std::fs::read_to_string(root().join("include/mpegts.h")).unwrap();
    text.lines()
        .filter_map(|l| {
            let mut parts = l.split_whitespace();
            (parts.next()? == "#define").then_some(())?;
            let name = parts.next()?.to_string();
            let raw = parts.next()?.trim_matches(|c| c == '(' || c == ')');
            let value = match raw.strip_prefix("0x") {
                Some(hex) => i64::from_str_radix(hex, 16).ok()?,
                None => raw.parse().ok()?,
            };
            Some((name, value))
        })
        .collect()
}

#[test]
fn header_constants_match_rust() {
    let defs = header_defines();
    for (name, value) in [
        ("MPEGTS_CARRIAGE_PES", MPEGTS_CARRIAGE_PES),
        ("MPEGTS_CARRIAGE_METADATA_CELLS", MPEGTS_CARRIAGE_METADATA_CELLS),
        ("MPEGTS_CARRIAGE_ASYNC_KLV", MPEGTS_CARRIAGE_ASYNC_KLV),
        ("MPEGTS_SPEC_SYNC_REGISTRATION", MPEGTS_SPEC_SYNC_REGISTRATION),
        ("MPEGTS_PES_HAS_PTS", MPEGTS_PES_HAS_PTS),
        ("MPEGTS_PES_HAS_DTS", MPEGTS_PES_HAS_DTS),
        ("MPEGTS_PES_HAS_PCR", MPEGTS_PES_HAS_PCR),
        ("MPEGTS_PES_DATA_ALIGNMENT", MPEGTS_PES_DATA_ALIGNMENT),
        ("MPEGTS_PES_RANDOM_ACCESS", MPEGTS_PES_RANDOM_ACCESS),
        ("MPEGTS_PES_ES_PRIORITY", MPEGTS_PES_ES_PRIORITY),
        ("MPEGTS_PES_HAS_SERVICE_ID", MPEGTS_PES_HAS_SERVICE_ID),
    ] {
        assert_eq!(defs.get(name), Some(&i64::from(value)), "{name}");
    }
    for prefix in ["MPEGTS_CARRIAGE_", "MPEGTS_SPEC_", "MPEGTS_PES_"] {
        let n = defs.keys().filter(|k| k.starts_with(prefix)).count();
        let expected = match prefix {
            "MPEGTS_CARRIAGE_" => 3,
            "MPEGTS_SPEC_" => 1,
            _ => 7,
        };
        assert_eq!(n, expected, "{prefix}* constants the Rust side does not know");
    }
    // The injector.
    let inject = [
        ("MPEGTS_INJECT_SYNC", MPEGTS_INJECT_SYNC),
        ("MPEGTS_INJECT_ASYNC", MPEGTS_INJECT_ASYNC),
        ("MPEGTS_INJECT_HAS_SERVICE_ID", MPEGTS_INJECT_HAS_SERVICE_ID),
        ("MPEGTS_INJECT_HAS_PID", MPEGTS_INJECT_HAS_PID),
        ("MPEGTS_INJECT_NO_FRAME_ALIGN", MPEGTS_INJECT_NO_FRAME_ALIGN),
        ("MPEGTS_INJECT_SYNC_REGISTRATION", MPEGTS_INJECT_SYNC_REGISTRATION),
        ("MPEGTS_INJECT_UNIT_HAS_PTS", MPEGTS_INJECT_UNIT_HAS_PTS),
        ("MPEGTS_EXISTING_KLV_KEEP", MPEGTS_EXISTING_KLV_KEEP),
        ("MPEGTS_EXISTING_KLV_REPLACE", MPEGTS_EXISTING_KLV_REPLACE),
        ("MPEGTS_EXISTING_KLV_FAIL", MPEGTS_EXISTING_KLV_FAIL),
        ("MPEGTS_RATE_VBR", MPEGTS_RATE_VBR),
        ("MPEGTS_RATE_CBR_FAIL", MPEGTS_RATE_CBR_FAIL),
        ("MPEGTS_RATE_CBR_DROP_OLDEST", MPEGTS_RATE_CBR_DROP_OLDEST),
        ("MPEGTS_FRAME_HAS_DTS", MPEGTS_FRAME_HAS_DTS),
        ("MPEGTS_FRAME_HAS_CLOCK", MPEGTS_FRAME_HAS_CLOCK),
        ("MPEGTS_FRAME_RANDOM_ACCESS", MPEGTS_FRAME_RANDOM_ACCESS),
        ("MPEGTS_FRAME_DISCONTINUITY", MPEGTS_FRAME_DISCONTINUITY),
        ("MPEGTS_TARGET_MERGED", MPEGTS_TARGET_MERGED),
        ("MPEGTS_TARGET_HAS_SERVICE_ID", MPEGTS_TARGET_HAS_SERVICE_ID),
        ("MPEGTS_EVENT_TARGET_FOUND", MPEGTS_EVENT_TARGET_FOUND),
        ("MPEGTS_EVENT_TARGET_CHANGED", MPEGTS_EVENT_TARGET_CHANGED),
        ("MPEGTS_EVENT_TARGET_LOST", MPEGTS_EVENT_TARGET_LOST),
        ("MPEGTS_EVENT_DISCONTINUITY", MPEGTS_EVENT_DISCONTINUITY),
        ("MPEGTS_EVENT_DROPPED", MPEGTS_EVENT_DROPPED),
        ("MPEGTS_EVENT_WARNING", MPEGTS_EVENT_WARNING),
        ("MPEGTS_EVENT_HAS_PTS", MPEGTS_EVENT_HAS_PTS),
        ("MPEGTS_DROP_LATE", MPEGTS_DROP_LATE),
        ("MPEGTS_DROP_OVERFLOW", MPEGTS_DROP_OVERFLOW),
        ("MPEGTS_DROP_DISCONTINUITY", MPEGTS_DROP_DISCONTINUITY),
        ("MPEGTS_DROP_UNPLACED", MPEGTS_DROP_UNPLACED),
        ("MPEGTS_WARN_MULTI_PROGRAM_VBR", MPEGTS_WARN_MULTI_PROGRAM_VBR),
        ("MPEGTS_WARN_SERVICE_ID_CHANGED", MPEGTS_WARN_SERVICE_ID_CHANGED),
        ("MPEGTS_WARN_SERVICE_ID_IN_INPUT", MPEGTS_WARN_SERVICE_ID_IN_INPUT),
        ("MPEGTS_WARN_LATE_UNIT", MPEGTS_WARN_LATE_UNIT),
    ];
    for (name, value) in inject {
        assert_eq!(defs.get(name), Some(&i64::from(value)), "{name}");
    }
    for prefix in [
        "MPEGTS_INJECT_",
        "MPEGTS_EXISTING_KLV_",
        "MPEGTS_RATE_",
        "MPEGTS_FRAME_",
        "MPEGTS_TARGET_",
        "MPEGTS_EVENT_",
        "MPEGTS_DROP_",
        "MPEGTS_WARN_",
    ] {
        let in_header = defs.keys().filter(|k| k.starts_with(prefix)).count();
        let in_rust = inject.iter().filter(|(k, _)| k.starts_with(prefix)).count();
        assert_eq!(in_header, in_rust, "{prefix}* constants the Rust side does not know");
    }
}

macro_rules! size {
    ($t:ident) => {
        (format!("S {}", stringify!($t)), std::mem::size_of::<$t>())
    };
}

macro_rules! field {
    ($t:ident, $f:ident) => {{
        let v = std::mem::MaybeUninit::<$t>::uninit();
        let base = v.as_ptr();
        // SAFETY: only the address is taken, nothing is read.
        let at = unsafe { std::ptr::addr_of!((*base).$f) } as usize - base as usize;
        (format!("L {}.{}", stringify!($t), stringify!($f)), at)
    }};
}

fn rust_layout() -> Vec<(String, usize)> {
    vec![
        size!(mpegts_stream_spec),
        size!(mpegts_pes_unit),
        field!(mpegts_stream_spec, struct_size),
        field!(mpegts_stream_spec, carriage),
        field!(mpegts_stream_spec, stream_type),
        field!(mpegts_stream_spec, stream_id),
        field!(mpegts_stream_spec, service_id),
        field!(mpegts_stream_spec, reserved),
        field!(mpegts_stream_spec, flags),
        field!(mpegts_stream_spec, input_leak_rate),
        field!(mpegts_stream_spec, buffer_size),
        field!(mpegts_stream_spec, output_leak_rate),
        field!(mpegts_pes_unit, struct_size),
        field!(mpegts_pes_unit, flags),
        field!(mpegts_pes_unit, pts),
        field!(mpegts_pes_unit, dts),
        field!(mpegts_pes_unit, pcr),
        field!(mpegts_pes_unit, data),
        field!(mpegts_pes_unit, data_len),
        field!(mpegts_pes_unit, service_id),
        field!(mpegts_pes_unit, reserved),
    ]
}

fn inject_layout() -> Vec<(String, usize)> {
    vec![
        size!(mpegts_inject_config),
        size!(mpegts_inject_unit),
        size!(mpegts_inject_frame),
        size!(mpegts_inject_target),
        size!(mpegts_inject_stats),
        size!(mpegts_inject_event),
        field!(mpegts_inject_config, struct_size),
        field!(mpegts_inject_config, flags),
        field!(mpegts_inject_config, program),
        field!(mpegts_inject_config, carriage),
        field!(mpegts_inject_config, existing_klv),
        field!(mpegts_inject_config, rate),
        field!(mpegts_inject_config, pid),
        field!(mpegts_inject_config, service_id),
        field!(mpegts_inject_config, reserved),
        field!(mpegts_inject_config, psi_interval_ms),
        field!(mpegts_inject_config, max_lead_ms),
        field!(mpegts_inject_config, min_lead_ms),
        field!(mpegts_inject_config, fallback_lead_ms),
        field!(mpegts_inject_config, input_leak_rate),
        field!(mpegts_inject_config, buffer_size),
        field!(mpegts_inject_config, output_leak_rate),
        field!(mpegts_inject_config, max_queued_units),
        field!(mpegts_inject_config, max_queued_bytes),
        field!(mpegts_inject_config, max_held_packets),
        field!(mpegts_inject_unit, struct_size),
        field!(mpegts_inject_unit, flags),
        field!(mpegts_inject_unit, pts),
        field!(mpegts_inject_unit, data),
        field!(mpegts_inject_unit, data_len),
        field!(mpegts_inject_frame, struct_size),
        field!(mpegts_inject_frame, flags),
        field!(mpegts_inject_frame, pid),
        field!(mpegts_inject_frame, reserved),
        field!(mpegts_inject_frame, reserved2),
        field!(mpegts_inject_frame, pts),
        field!(mpegts_inject_frame, dts),
        field!(mpegts_inject_frame, clock),
        field!(mpegts_inject_target, struct_size),
        field!(mpegts_inject_target, flags),
        field!(mpegts_inject_target, program),
        field!(mpegts_inject_target, pmt_pid),
        field!(mpegts_inject_target, pcr_pid),
        field!(mpegts_inject_target, video_pid),
        field!(mpegts_inject_target, klv_pid),
        field!(mpegts_inject_target, service_id),
        field!(mpegts_inject_target, carriage),
        field!(mpegts_inject_target, stripped_count),
        field!(mpegts_inject_target, stripped),
        field!(mpegts_inject_stats, struct_size),
        field!(mpegts_inject_stats, reserved),
        field!(mpegts_inject_stats, packets_in),
        field!(mpegts_inject_stats, packets_out),
        field!(mpegts_inject_stats, units_inserted),
        field!(mpegts_inject_stats, metadata_buffer_peak),
        field!(mpegts_inject_stats, added_bitrate),
        field!(mpegts_inject_stats, events_dropped),
        field!(mpegts_inject_event, struct_size),
        field!(mpegts_inject_event, kind),
        field!(mpegts_inject_event, code),
        field!(mpegts_inject_event, flags),
        field!(mpegts_inject_event, value),
        field!(mpegts_inject_event, bytes),
    ]
}

fn compiler() -> Option<&'static str> {
    ["cc", "gcc", "clang"].into_iter().find(|c| Command::new(c).arg("--version").output().is_ok())
}

/// The shared library as `make` builds it (with `mux`), in the same
/// target directory as tests/c_abi.rs uses.
fn build_library() -> PathBuf {
    let target = root().join("target/c-abi");
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let status = Command::new(cargo)
        .args(["build", "--lib", "--quiet", "--offline", "--features", "mux"])
        .env("CARGO_TARGET_DIR", &target)
        .current_dir(root())
        .status()
        .unwrap();
    assert!(status.success(), "cargo build of the C library failed");
    target.join("debug")
}

/// The fake KLV packet tests/c/mux_test.c builds.
fn klv(value_len: usize, fill: u8) -> Vec<u8> {
    let mut v = vec![0x06, 0x0E, 0x2B, 0x34, 0x02, 0x0B, 0x01, 0x01, 0x0E, 0x01, 0x03, 0x01, 0x01, 0x00, 0x00, 0x00];
    v.extend([0x83, (value_len >> 16) as u8, (value_len >> 8) as u8, value_len as u8]);
    v.resize(v.len() + value_len, fill);
    v
}

#[test]
fn c_program_writes_a_stream_the_demuxer_reads() {
    if cfg!(windows) {
        eprintln!("skipping: Unix only");
        return;
    }
    let Some(cc) = compiler() else {
        eprintln!("skipping: no C compiler");
        return;
    };
    let lib = build_library();
    let work = root().join("target/c-abi/work");
    std::fs::create_dir_all(&work).unwrap();
    let exe = work.join(format!("mux_test_{}", std::process::id()));
    let compiled = Command::new(cc)
        .args(["-std=c99", "-Wall", "-Wextra", "-Werror", "-pedantic", "-I"])
        .arg(root().join("include"))
        .arg(root().join("tests/c/mux_test.c"))
        .arg("-L")
        .arg(&lib)
        .arg("-lmpegts")
        .arg(format!("-Wl,-rpath,{}", lib.display()))
        .arg("-o")
        .arg(&exe)
        .status()
        .unwrap();
    assert!(compiled.success(), "C compile failed");
    let output = work.join(format!("mux_{}.ts", std::process::id()));
    let run = Command::new(&exe).arg(&output).output().unwrap();
    let _ = std::fs::remove_file(&exe);
    let stdout = String::from_utf8(run.stdout).unwrap();
    assert!(run.status.success(), "{stdout}\n{}", String::from_utf8_lossy(&run.stderr));
    let ts = std::fs::read(&output).unwrap();
    let _ = std::fs::remove_file(&output);

    // Layout.
    let c: HashMap<&str, usize> = stdout
        .lines()
        .filter(|l| l.starts_with("S ") || l.starts_with("L "))
        .map(|l| {
            let (k, v) = l.rsplit_once(' ').unwrap();
            (k, v.parse().unwrap())
        })
        .collect();
    let rust = rust_layout();
    assert_eq!(c.len(), rust.len());
    for (key, offset) in &rust {
        assert_eq!(c.get(key.as_str()), Some(offset), "{key}");
    }

    // What the calls returned.
    let lines: Vec<&str> = stdout.lines().filter(|l| !l.starts_with("S ") && !l.starts_with("L ")).collect();
    let expected = [
        "SPEC sync carriage=2 type=0x15 id=0xfc rate=0x3fffff buffer=0x000400".to_string(),
        "DUP -2".into(),
        "ERR PID 0x0102 already in use: two streams of one program (R-ISO13818-020)".into(),
        "MISSING -4".into(),
        "PMT_VERSION 3".into(),
        "PAT_VERSION 1".into(),
        "QUERY -7 376".into(),
        "SHORT -7 376".into(),
        "PAT_CC 0".into(),
        "ASYNC_PTS -2".into(),
        "SYNC_NO_PTS -2".into(),
        "ERR invalid unit: synchronous metadata needs a PTS in every PES packet (R-ST1402TIME-17)".into(),
        "SEQ 6".into(),
        "FROM_REPORT same".into(),
        "FROM_REPORT_PMT same 59".into(),
        format!("WROTE {}", ts.len()),
        "DONE".into(),
    ];
    assert_eq!(lines, expected);

    // The stream reads back exactly.
    let mut d = Demuxer::new();
    let sync = d.subscribe(Subscription::new(Select::Codec(Codec::KlvSync), Unit::Pes).strip_metadata_au()).unwrap();
    let asynchronous = d.subscribe(Subscription::new(Select::Codec(Codec::KlvAsync), Unit::Pes)).unwrap();
    let video = d.subscribe(Subscription::new(Select::Codec(Codec::H264), Unit::Pes)).unwrap();
    let psi = d.subscribe(Subscription::new(Select::Psi, Unit::Sections)).unwrap();
    d.push(&ts);
    d.finish();
    let report = d.report();
    assert_eq!(report.transport_stream_id, Some(7));
    assert_eq!(report.stats.crc_errors, 0);
    let p = report.program(1).unwrap();
    assert_eq!(p.pcr_pid, Some(0x100));
    let codecs: Vec<(u16, Codec)> = p.streams.iter().map(|s| (s.pid, s.codec)).collect();
    assert_eq!(codecs, [(0x100, Codec::H264), (0x101, Codec::KlvSync), (0x102, Codec::KlvAsync)]);
    assert!(report.streams().all(|s| s.cc_errors == 0));
    assert!(report.pids.iter().all(|p| p.cc_errors == 0));

    let outs: Vec<_> = d.outputs().collect();
    let of = |id| outs.iter().filter(move |o| o.subscription == id);
    assert!(of(psi).all(|o| o.crc_ok == Some(true)));
    assert_eq!(of(psi).count(), 10);
    let pts = |i: u64| 126_000 + i * 3003 + 3003;

    // Sync KLV: frame 2 is fragmented into two PES packets with one PTS.
    let sync_outs: Vec<_> = of(sync).collect();
    assert_eq!(sync_outs.len(), 6);
    let mut units: Vec<(u64, Vec<u8>)> = Vec::new();
    for o in &sync_outs {
        assert!(!o.damaged);
        match units.last_mut() {
            Some((p, data)) if Some(*p) == o.pts => data.extend_from_slice(o.payload()),
            _ => units.push((o.pts.unwrap(), o.payload().to_vec())),
        }
    }
    for (i, (p, data)) in units.iter().enumerate() {
        let i = i as u64;
        assert_eq!(*p, pts(i));
        assert_eq!(*data, klv(if i == 2 { 70_000 } else { 100 + i as usize }, i as u8));
    }

    // Async KLV: no PTS, one PES per item, data_alignment_indicator set.
    let async_outs: Vec<_> = of(asynchronous).collect();
    assert_eq!(async_outs.len(), 5);
    for (i, o) in async_outs.iter().enumerate() {
        assert_eq!(o.pts, None);
        assert_eq!(o.payload(), klv(50 + i, 0x40 + i as u8));
        let h = PesHeader::parse(&o.data).unwrap();
        assert!(h.data_alignment && h.original);
    }

    // Video: PTS and DTS.
    let video_outs: Vec<_> = of(video).collect();
    assert_eq!(video_outs.len(), 5);
    for (i, o) in video_outs.iter().enumerate() {
        assert_eq!((o.pts, o.dts), (Some(pts(i as u64)), Some(pts(i as u64) - 3003)));
        assert_eq!(o.payload().len(), 3000);
    }
}

/// A small input for the C injector test: program 1 with H.264 video on
/// 0x100 carrying the PCR, PAT and PMT every frame, 30 frames.
fn video_stream() -> (Vec<u8>, Vec<u64>) {
    use mpegts::mux::{PesUnit, PesWriter, ProgramTables, StreamSpec};
    let mut tables = ProgramTables::new(1);
    tables.add_program(1, 0x1000, Some(0x100)).unwrap();
    tables.add_stream(1, 0x100, &StreamSpec::h264()).unwrap();
    let mut video = PesWriter::new(0x100, &StreamSpec::h264()).unwrap();
    let mut ts = Vec::new();
    let mut pts = Vec::new();
    for i in 0..30u64 {
        tables.write_all(&mut ts).unwrap();
        let p = 900_000 + i * 3003;
        let frame = vec![0x5Au8; 2000];
        video.write(&PesUnit::new(&frame).with_pts(p).with_pcr((p - 36_000) * 300), &mut ts).unwrap();
        pts.push(p);
    }
    (ts, pts)
}

#[test]
fn c_program_injects_klv() {
    if cfg!(windows) {
        eprintln!("skipping: Unix only");
        return;
    }
    let Some(cc) = compiler() else {
        eprintln!("skipping: no C compiler");
        return;
    };
    let lib = build_library();
    let work = root().join("target/c-abi/work");
    std::fs::create_dir_all(&work).unwrap();
    let exe = work.join(format!("inject_test_{}", std::process::id()));
    let compiled = Command::new(cc)
        .args(["-std=c99", "-Wall", "-Wextra", "-Werror", "-pedantic", "-I"])
        .arg(root().join("include"))
        .arg(root().join("tests/c/inject_test.c"))
        .arg("-L")
        .arg(&lib)
        .arg("-lmpegts")
        .arg(format!("-Wl,-rpath,{}", lib.display()))
        .arg("-o")
        .arg(&exe)
        .status()
        .unwrap();
    assert!(compiled.success(), "C compile failed");
    let (input, frame_pts) = video_stream();
    let in_path = work.join(format!("inject_in_{}.ts", std::process::id()));
    let out_path = work.join(format!("inject_out_{}.ts", std::process::id()));
    std::fs::write(&in_path, &input).unwrap();
    let run = Command::new(&exe).arg(&in_path).arg(&out_path).output().unwrap();
    let _ = std::fs::remove_file(&exe);
    let _ = std::fs::remove_file(&in_path);
    let stdout = String::from_utf8(run.stdout).unwrap();
    assert!(run.status.success(), "{stdout}\n{}", String::from_utf8_lossy(&run.stderr));
    let ts = std::fs::read(&out_path).unwrap();
    let _ = std::fs::remove_file(&out_path);

    let c: HashMap<&str, usize> = stdout
        .lines()
        .filter(|l| l.starts_with("S ") || l.starts_with("L "))
        .map(|l| {
            let (k, v) = l.rsplit_once(' ').unwrap();
            (k, v.parse().unwrap())
        })
        .collect();
    let rust = inject_layout();
    assert_eq!(c.len(), rust.len());
    for (key, offset) in &rust {
        assert_eq!(c.get(key.as_str()), Some(offset), "{key}");
    }

    let lines: Vec<&str> = stdout.lines().filter(|l| !l.starts_with("S ") && !l.starts_with("L ")).collect();
    let packets = input.len() / 188;
    let expected = [
        "DEFAULTS psi=100 lead=1..990 carriage=1 rate=0 units=4096".to_string(),
        "CHECK -2 invalid injector configuration: psi_interval_ms must be 1 to 249 (ST 1402.2 asks for more than 4 PSI repetitions per second)".into(),
        "NEW_BAD -2 null".into(),
        "TARGET_BEFORE 1".into(),
        "CLOCK_BEFORE 1".into(),
        "NO_PTS -2".into(),
        "ERR invalid unit: synchronous KLV needs a PTS (R-ST1402TIME-17)".into(),
        "NO_SINK -1".into(),
        "PUSH_AFTER_END -2".into(),
        "FRAMES 30 clock=30 inserted=30 klv_pid=0x0101".into(),
        format!("STATS in={packets} out={} units=30 late=0 out_bytes=match", ts.len() / 188),
        "TARGET program=1 pmt=0x1000 pcr=0x0100 video=0x0100 klv=0x0101 service=0 carriage=1 flags=2".into(),
        "EVENT kind=1 code=0 value=1".into(),
        "CLOCK set".into(),
        "AFTER_FINISH -2".into(),
        "STATUS queue full; try again later / the input cannot be processed as configured".into(),
        "DONE".into(),
    ];
    assert_eq!(lines, expected);

    // The output reads back: one KLV unit per frame, with its PTS.
    let mut d = Demuxer::new();
    let klv = d.subscribe(Subscription::new(Select::Codec(Codec::KlvSync), Unit::Pes).strip_metadata_au()).unwrap();
    d.push(&ts);
    d.finish();
    let report = d.report();
    assert!(report.pids.iter().all(|p| p.cc_errors == 0));
    let units: Vec<_> = d.outputs().filter(|o| o.subscription == klv).collect();
    assert_eq!(units.len(), 30);
    for (u, p) in units.iter().zip(&frame_pts) {
        assert_eq!(u.pts, Some(*p));
        assert_eq!(u.payload()[17..25], p.to_be_bytes());
    }
}
