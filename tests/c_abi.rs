//! The C ABI as a C compiler sees it: header constants, struct layout,
//! and a C program linked against the shared library.
mod common;

use common::*;
use mpegts::capi::*;
use mpegts::{Basis, Codec, StreamKind};
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::process::Command;

fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

/// `#define NAME VALUE` pairs from the header.
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

/// C constant name for a Rust variant name: `AacAdts` -> `AAC_ADTS`.
fn c_name(rust: &str) -> String {
    let mut out = String::new();
    let mut prev_lower_or_digit = false;
    for ch in rust.chars() {
        if ch.is_ascii_uppercase() && prev_lower_or_digit {
            out.push('_');
        }
        prev_lower_or_digit = ch.is_ascii_lowercase() || ch.is_ascii_digit();
        out.push(ch.to_ascii_uppercase());
    }
    out
}

#[test]
fn header_constants_match_rust() {
    let defs = header_defines();
    let get = |name: &str| *defs.get(name).unwrap_or_else(|| panic!("{name} missing from header"));
    for c in Codec::ALL {
        assert_eq!(get(&format!("MPEGTS_CODEC_{}", c_name(&format!("{c:?}")))), i64::from(c.code()), "{c:?}");
    }
    let codec_defs = defs.keys().filter(|k| k.starts_with("MPEGTS_CODEC_")).count();
    assert_eq!(codec_defs, Codec::ALL.len(), "header has codecs Rust does not know");
    for k in StreamKind::ALL {
        assert_eq!(get(&format!("MPEGTS_KIND_{}", c_name(&format!("{k:?}")))), i64::from(k.code()));
    }
    for b in [Basis::StreamType, Basis::Descriptor, Basis::Payload] {
        assert_eq!(get(&format!("MPEGTS_BASIS_{}", c_name(&format!("{b:?}")))), i64::from(b.code()));
    }
    for (name, value) in [
        ("MPEGTS_ABI_VERSION", i64::from(MPEGTS_ABI_VERSION)),
        ("MPEGTS_OK", MPEGTS_OK.into()),
        ("MPEGTS_EMPTY", MPEGTS_EMPTY.into()),
        ("MPEGTS_ERR_NULL", MPEGTS_ERR_NULL.into()),
        ("MPEGTS_ERR_INVALID", MPEGTS_ERR_INVALID.into()),
        ("MPEGTS_ERR_RANGE", MPEGTS_ERR_RANGE.into()),
        ("MPEGTS_ERR_NOT_FOUND", MPEGTS_ERR_NOT_FOUND.into()),
        ("MPEGTS_ERR_PANIC", MPEGTS_ERR_PANIC.into()),
        ("MPEGTS_ERR_POISONED", MPEGTS_ERR_POISONED.into()),
        ("MPEGTS_ERR_BUFFER", MPEGTS_ERR_BUFFER.into()),
        ("MPEGTS_ERR_FULL", MPEGTS_ERR_FULL.into()),
        ("MPEGTS_ERR_STREAM", MPEGTS_ERR_STREAM.into()),
        ("MPEGTS_UNIT_PACKETS", mpegts::Unit::Packets as i64),
        ("MPEGTS_UNIT_PES", mpegts::Unit::Pes as i64),
        ("MPEGTS_UNIT_SECTIONS", mpegts::Unit::Sections as i64),
        ("MPEGTS_VERSION_MAJOR", env!("CARGO_PKG_VERSION_MAJOR").parse().unwrap()),
        ("MPEGTS_VERSION_MINOR", env!("CARGO_PKG_VERSION_MINOR").parse().unwrap()),
        ("MPEGTS_VERSION_PATCH", env!("CARGO_PKG_VERSION_PATCH").parse().unwrap()),
    ] {
        assert_eq!(get(name), value, "{name}");
    }
    let header = std::fs::read_to_string(root().join("include/mpegts.h")).unwrap();
    let version = format!("#define MPEGTS_VERSION_STRING \"{}\"", env!("CARGO_PKG_VERSION"));
    assert!(header.contains(&version), "header version string is not {}", env!("CARGO_PKG_VERSION"));
}

/// Function names the header declares.
fn header_functions() -> BTreeSet<String> {
    let text = std::fs::read_to_string(root().join("include/mpegts.h")).unwrap();
    let mut names = BTreeSet::new();
    for line in text.lines().filter(|l| !l.trim_start().starts_with(['#', '*', '/'])) {
        let mut rest = line;
        while let Some(at) = rest.find("mpegts_") {
            let tail = &rest[at..];
            let end = tail.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).unwrap_or(tail.len());
            if tail[end..].starts_with('(') {
                names.insert(tail[..end].to_string());
            }
            rest = &tail[end..];
        }
    }
    names
}

#[test]
fn exported_symbols_match_header() {
    // `nm -D` reads ELF dynamic symbols, so Linux only.
    if !cfg!(target_os = "linux") || Command::new("nm").arg("--version").output().is_err() {
        eprintln!("skipping: needs Linux and nm");
        return;
    }
    let exported = dynamic_symbols(&build_library().join("libmpegts.so"));
    let declared = header_functions();
    // 29 for reading, 37 for writing and 20 for injecting (built with the
    // `mux` feature).
    assert_eq!(declared.len(), 87, "header functions: {declared:?}");
    let missing: Vec<_> = declared.difference(&exported).collect();
    let extra: Vec<_> = exported.difference(&declared).collect();
    assert!(missing.is_empty(), "declared in the header but not exported: {missing:?}");
    assert!(extra.is_empty(), "exported but not in the header: {extra:?}");
}

/// Write-side functions (feature `mux`).
fn is_write_side(name: &str) -> bool {
    [
        "mpegts_program_tables_",
        "mpegts_pes_writer_",
        "mpegts_stream_spec_",
        "mpegts_null_packet",
        "mpegts_inject_",
        "mpegts_injector_",
    ]
    .iter()
    .any(|p| name.starts_with(p))
}

fn dynamic_symbols(lib: &Path) -> BTreeSet<String> {
    let out = Command::new("nm").args(["-D", "--defined-only"]).arg(lib).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .filter_map(|l| l.split_whitespace().last())
        .map(|s| s.trim_start_matches('_').to_string())
        .collect()
}

/// A build with default features compiles none of the write side: the C
/// library exports only the reading functions, and the Rust library holds
/// no `mpegts::mux` code.
#[test]
fn default_build_has_no_write_side() {
    if !cfg!(target_os = "linux") || Command::new("nm").arg("--version").output().is_err() {
        eprintln!("skipping: needs Linux and nm");
        return;
    }
    let target = root().join("target/c-abi-default");
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let status = Command::new(cargo)
        .args(["build", "--lib", "--quiet", "--offline"])
        .env("CARGO_TARGET_DIR", &target)
        .current_dir(root())
        .status()
        .unwrap();
    assert!(status.success(), "cargo build with default features failed");
    let exported = dynamic_symbols(&target.join("debug/libmpegts.so"));
    let reading: BTreeSet<String> = header_functions().into_iter().filter(|f| !is_write_side(f)).collect();
    assert_eq!(reading.len(), 29);
    assert_eq!(exported, reading, "a default build exports exactly the reading functions");
    let rlib = Command::new("nm").arg(target.join("debug/libmpegts.rlib")).output().unwrap();
    let symbols = String::from_utf8_lossy(&rlib.stdout);
    assert!(symbols.contains("6mpegts7demuxer"), "nm did not list the rlib's symbols");
    assert!(!symbols.contains("6mpegts3mux"), "mpegts::mux code in a default build");
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
        size!(mpegts_config),
        size!(mpegts_subscription),
        size!(mpegts_output),
        size!(mpegts_output_stats),
        size!(mpegts_report_summary),
        size!(mpegts_program_info),
        size!(mpegts_stream_info),
        size!(mpegts_pid_info),
        field!(mpegts_config, struct_size),
        field!(mpegts_config, sync_confirmations),
        field!(mpegts_config, probe_pes),
        field!(mpegts_config, overflow),
        field!(mpegts_config, max_streams),
        field!(mpegts_config, history_packets),
        field!(mpegts_config, queue_max_items),
        field!(mpegts_config, queue_max_bytes),
        field!(mpegts_config, max_pes_bytes),
        field!(mpegts_config, max_reassembly_bytes),
        field!(mpegts_subscription, struct_size),
        field!(mpegts_subscription, select),
        field!(mpegts_subscription, value),
        field!(mpegts_subscription, unit),
        field!(mpegts_subscription, replay),
        field!(mpegts_subscription, strip_metadata_au),
        field!(mpegts_output, struct_size),
        field!(mpegts_output, subscription),
        field!(mpegts_output, unit),
        field!(mpegts_output, flags),
        field!(mpegts_output, pid),
        field!(mpegts_output, stream_id),
        field!(mpegts_output, table_id),
        field!(mpegts_output, reserved),
        field!(mpegts_output, packet),
        field!(mpegts_output, pts),
        field!(mpegts_output, dts),
        field!(mpegts_output, data),
        field!(mpegts_output, data_len),
        field!(mpegts_output, payload_offset),
        field!(mpegts_output_stats, struct_size),
        field!(mpegts_output_stats, subscriptions),
        field!(mpegts_output_stats, queued),
        field!(mpegts_output_stats, dropped),
        field!(mpegts_output_stats, truncated),
        field!(mpegts_output_stats, damaged),
        field!(mpegts_output_stats, pending),
        field!(mpegts_output_stats, pending_bytes),
        field!(mpegts_output_stats, reassembly_bytes),
        field!(mpegts_report_summary, struct_size),
        field!(mpegts_report_summary, flags),
        field!(mpegts_report_summary, revision),
        field!(mpegts_report_summary, transport_stream_id),
        field!(mpegts_report_summary, network_pid),
        field!(mpegts_report_summary, pat_version),
        field!(mpegts_report_summary, reserved),
        field!(mpegts_report_summary, packet_size),
        field!(mpegts_report_summary, program_count),
        field!(mpegts_report_summary, pid_count),
        field!(mpegts_report_summary, ca_system_count),
        field!(mpegts_report_summary, packets),
        field!(mpegts_report_summary, bytes_skipped),
        field!(mpegts_report_summary, sync_losses),
        field!(mpegts_report_summary, crc_errors),
        field!(mpegts_report_summary, streams_dropped),
        field!(mpegts_report_summary, duration_ns),
        field!(mpegts_report_summary, bitrate),
        field!(mpegts_program_info, struct_size),
        field!(mpegts_program_info, flags),
        field!(mpegts_program_info, number),
        field!(mpegts_program_info, pmt_pid),
        field!(mpegts_program_info, pcr_pid),
        field!(mpegts_program_info, version),
        field!(mpegts_program_info, reserved),
        field!(mpegts_program_info, registration),
        field!(mpegts_program_info, stream_count),
        field!(mpegts_program_info, descriptor_count),
        field!(mpegts_program_info, reserved2),
        field!(mpegts_program_info, first_pcr),
        field!(mpegts_program_info, last_pcr),
        field!(mpegts_program_info, pcr_span),
        field!(mpegts_stream_info, struct_size),
        field!(mpegts_stream_info, flags),
        field!(mpegts_stream_info, program),
        field!(mpegts_stream_info, pid),
        field!(mpegts_stream_info, stream_type),
        field!(mpegts_stream_info, pes_stream_id),
        field!(mpegts_stream_info, reserved),
        field!(mpegts_stream_info, codec),
        field!(mpegts_stream_info, kind),
        field!(mpegts_stream_info, basis),
        field!(mpegts_stream_info, language),
        field!(mpegts_stream_info, registration),
        field!(mpegts_stream_info, metadata_format),
        field!(mpegts_stream_info, descriptor_count),
        field!(mpegts_stream_info, reserved2),
        field!(mpegts_stream_info, first_pts),
        field!(mpegts_stream_info, last_pts),
        field!(mpegts_stream_info, last_dts),
        field!(mpegts_stream_info, pts_span),
        field!(mpegts_stream_info, packets),
        field!(mpegts_stream_info, cc_errors),
        field!(mpegts_stream_info, scrambled_packets),
        field!(mpegts_stream_info, last_packet),
        field!(mpegts_stream_info, bitrate),
        field!(mpegts_pid_info, struct_size),
        field!(mpegts_pid_info, pid),
        field!(mpegts_pid_info, reserved),
        field!(mpegts_pid_info, packets),
        field!(mpegts_pid_info, cc_errors),
        field!(mpegts_pid_info, transport_errors),
        field!(mpegts_pid_info, scrambled_packets),
        field!(mpegts_pid_info, last_packet),
    ]
}

fn compiler() -> Option<&'static str> {
    ["cc", "gcc", "clang"].into_iter().find(|c| Command::new(c).arg("--version").output().is_ok())
}

/// Build the shared library into its own target directory (so it does not
/// wait on the cargo running this test) and return where it is. It is
/// built as `make` builds it, with the `mux` feature, so that it exports
/// everything the header declares.
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

#[test]
fn c_program_layout_and_behaviour() {
    // The linking below is Unix-style; Windows needs an MSVC setup.
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
    let exe = work.join(format!("abi_test_{}", std::process::id()));
    let compiled = Command::new(cc)
        .args(["-std=c99", "-Wall", "-Wextra", "-Werror", "-pedantic", "-I"])
        .arg(root().join("include"))
        .arg(root().join("tests/c/abi_test.c"))
        .arg("-L")
        .arg(&lib)
        .arg("-lmpegts")
        .arg(format!("-Wl,-rpath,{}", lib.display()))
        .arg("-o")
        .arg(&exe)
        .status()
        .unwrap();
    assert!(compiled.success(), "C compile failed");

    // A stream with two KLV PES and a language-tagged video stream.
    let mut ts = Ts::new();
    ts.section(0, &pat(5, 0, &[(1, 0x1000)]));
    ts.section(0x1000, &pmt(1, 0, 0x100, &[], &[(H264, 0x100, language(b"spa")), (PRIVATE, 0x102, registration(b"KLVA"))]));
    ts.pcr(0x100, 0);
    for i in 0..2u64 {
        ts.pes(0x100, 0xE0, Some(i * 3003), &[0; 400]);
        ts.pes(0x102, 0xBD, Some(500 + i), &KLV_KEY);
    }
    ts.pcr(0x100, 27_000_000 * 2);
    let input = work.join(format!("input_{}.ts", std::process::id()));
    std::fs::write(&input, &ts.out).unwrap();

    let run = Command::new(&exe).arg(&input).output().unwrap();
    let _ = std::fs::remove_file(&exe);

    // The shipped C example builds cleanly and works too.
    let example = work.join(format!("klv_dump_{}", std::process::id()));
    let compiled = Command::new(cc)
        .args(["-std=c99", "-Wall", "-Wextra", "-Werror", "-pedantic", "-I"])
        .arg(root().join("include"))
        .arg(root().join("examples/c/klv_dump.c"))
        .arg("-L")
        .arg(&lib)
        .arg("-lmpegts")
        .arg(format!("-Wl,-rpath,{}", lib.display()))
        .arg("-o")
        .arg(&example)
        .status()
        .unwrap();
    assert!(compiled.success(), "examples/c/klv_dump.c failed to compile");
    let dump = Command::new(&example).arg(&input).output().unwrap();
    let _ = std::fs::remove_file(&example);
    let _ = std::fs::remove_file(&input);
    assert!(dump.status.success());
    assert_eq!(
        String::from_utf8(dump.stdout).unwrap(),
        "pid 0x0102  16 bytes  pts 500 (0.006 s)\npid 0x0102  16 bytes  pts 501 (0.006 s)\n"
    );
    let stdout = String::from_utf8(run.stdout).unwrap();
    assert!(run.status.success(), "{stdout}\n{}", String::from_utf8_lossy(&run.stderr));

    // Layout: every struct size and field offset as C computes them.
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

    // Behaviour.
    let lines: Vec<&str> = stdout.lines().filter(|l| !l.starts_with("S ") && !l.starts_with("L ")).collect();
    let expected = [
        format!("ABI 1 VERSION {}", env!("CARGO_PKG_VERSION")),
        "CHECK -2 invalid config: max_pes_bytes must be at least 188".into(),
        "SUB klv=1".into(),
        "SUB tables=2".into(),
        "OUT sub=2 unit=3 pid=0 pts=0 has_pts=0 len=16 payload=0 first=00 crc_ok=1".into(),
        "OUT sub=2 unit=3 pid=4096 pts=0 has_pts=0 len=38 payload=0 first=02 crc_ok=1".into(),
        "OUT sub=1 unit=2 pid=258 pts=500 has_pts=1 len=30 payload=14 first=06 crc_ok=0".into(),
        "OUT sub=1 unit=2 pid=258 pts=501 has_pts=1 len=30 payload=14 first=06 crc_ok=0".into(),
        "SUMMARY tsid=5 programs=1 packets=12 packet_size=188 duration_ns=2000000000".into(),
        "PROGRAM 1 pmt=4096 pcr=256 streams=2".into(),
        "STREAM pid=256 codec=H.264/AVC kind=video lang=spa packets=8".into(),
        "STREAM pid=258 codec=KLV (async) kind=metadata lang=- packets=2".into(),
        "ANALYZE programs=1 packets=12".into(),
        "DONE".into(),
    ];
    assert_eq!(lines, expected);
}

#[test]
fn header_compiles_as_cpp() {
    if cfg!(windows) || Command::new("c++").arg("--version").output().is_err() {
        eprintln!("skipping: no C++ compiler");
        return;
    }
    let status = Command::new("c++")
        .args(["-std=c++11", "-Wall", "-Wextra", "-Werror", "-pedantic", "-fsyntax-only", "-x", "c++"])
        .arg(root().join("include/mpegts.h"))
        .status()
        .unwrap();
    assert!(status.success());
}
