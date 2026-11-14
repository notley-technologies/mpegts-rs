//! Query a report programmatically and dump it as JSON.
//!
//!     cargo run --release --features serde --example json -- capture.ts

use mpegts::{Codec, StreamKind};
use std::{env, fs, process};

fn main() {
    let Some(path) = env::args().nth(1) else {
        eprintln!("usage: json <file.ts>");
        process::exit(2);
    };
    let data = fs::read(&path).unwrap_or_else(|e| {
        eprintln!("{path}: {e}");
        process::exit(1);
    });
    let report = mpegts::analyze(&data);

    // A few typical queries.
    eprintln!("video streams: {}", report.count(StreamKind::Video));
    for v in report.video() {
        eprintln!("  pid 0x{:04x}: {}", v.pid, v.codec);
    }
    eprintln!("has H.264: {}", report.has(Codec::H264));
    for k in report.klv() {
        eprintln!("KLV on pid 0x{:04x} ({:?}, from {:?})", k.pid, k.codec, k.basis);
    }
    if let Some(d) = report.duration() {
        eprintln!("duration: {:.3}s", d.as_secs_f64());
    }

    println!("{}", serde_json::to_string_pretty(&report).unwrap());
}
