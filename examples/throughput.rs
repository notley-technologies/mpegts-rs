//! Throughput benchmark with run statistics.
//!
//!     cargo run --release --example throughput -- [FILE] [options]
//!
//! FILE                 transport stream to measure (default: a 376 MB synthetic stream)
//! --runs N             measured runs per chunk size (default 10)
//! --warmup N           unmeasured runs before measuring (default 1)
//! --chunk LIST         comma-separated push sizes in bytes, or `whole`
//!                      (default: whole,1316,65536; 1316 = one UDP datagram)
//! --extract MODE       also extract data while measuring:
//!                      none (default), klv, pes, packets, sections
//!
//! The file is read into memory first, so disk speed is not measured.
//! Every run must produce the same report; the benchmark stops if not.

use mpegts::{Codec, Demuxer, Report, Select, Subscription, Unit};
use std::time::{Duration, Instant};
use std::{env, fs, process};

struct Options {
    file: Option<String>,
    runs: usize,
    warmup: usize,
    chunks: Vec<Option<usize>>,
    extract: String,
}

fn usage(msg: &str) -> ! {
    eprintln!("{msg}\nusage: throughput [FILE] [--runs N] [--warmup N] [--chunk whole,1316,...] [--extract none|klv|pes|packets|sections]");
    process::exit(2);
}

fn parse_args() -> Options {
    let mut o = Options {
        file: None,
        runs: 10,
        warmup: 1,
        chunks: vec![None, Some(1316), Some(65536)],
        extract: "none".into(),
    };
    let mut args = env::args().skip(1);
    while let Some(a) = args.next() {
        let mut value = |name: &str| args.next().unwrap_or_else(|| usage(&format!("{name} needs a value")));
        match a.as_str() {
            "--runs" => o.runs = value("--runs").parse().unwrap_or_else(|_| usage("--runs must be a number")),
            "--warmup" => o.warmup = value("--warmup").parse().unwrap_or_else(|_| usage("--warmup must be a number")),
            "--chunk" => {
                o.chunks = value("--chunk")
                    .split(',')
                    .map(|c| match c.trim() {
                        "whole" => None,
                        n => match n.parse::<usize>() {
                            Ok(n) if n > 0 => Some(n),
                            _ => usage("--chunk sizes must be positive numbers or `whole`"),
                        },
                    })
                    .collect()
            }
            "--extract" => o.extract = value("--extract"),
            "-h" | "--help" => usage("Measures demuxer throughput."),
            f if !f.starts_with('-') && o.file.is_none() => o.file = Some(f.to_string()),
            other => usage(&format!("unknown argument {other}")),
        }
    }
    if o.runs == 0 {
        usage("--runs must be at least 1");
    }
    if !["none", "klv", "pes", "packets", "sections"].contains(&o.extract.as_str()) {
        usage("--extract must be none, klv, pes, packets or sections");
    }
    o
}

/// One full pass over `data`. Returns the time, the report and how many
/// outputs were extracted.
fn run(data: &[u8], chunk: Option<usize>, extract: &str) -> (Duration, Report, u64) {
    let mut d = Demuxer::new();
    let subs: &[(Select, Unit)] = match extract {
        "klv" => &[(Select::Codec(Codec::KlvAsync), Unit::Pes), (Select::Codec(Codec::KlvSync), Unit::Pes)],
        "pes" => &[(Select::All, Unit::Pes)],
        "packets" => &[(Select::All, Unit::Packets)],
        "sections" => &[(Select::Psi, Unit::Sections)],
        _ => &[],
    };
    for &(select, unit) in subs {
        d.subscribe(Subscription::new(select, unit)).unwrap();
    }
    let mut outputs = 0;
    let mut drain = |d: &mut Demuxer| {
        while let Some(o) = d.pop_output() {
            outputs += 1;
            d.recycle(o);
        }
    };
    let start = Instant::now();
    match chunk {
        None => {
            d.push(data);
            drain(&mut d);
        }
        Some(n) => {
            for c in data.chunks(n) {
                d.push(c);
                drain(&mut d);
            }
        }
    }
    d.finish();
    drain(&mut d);
    let elapsed = start.elapsed();
    (elapsed, d.report(), outputs)
}

/// Nearest-rank percentile of sorted values.
fn percentile(sorted: &[f64], p: f64) -> f64 {
    let rank = ((p / 100.0) * sorted.len() as f64).ceil() as usize;
    sorted[rank.clamp(1, sorted.len()) - 1]
}

fn main() {
    let o = parse_args();
    let (data, label) = match &o.file {
        Some(path) => (
            fs::read(path).unwrap_or_else(|e| {
                eprintln!("{path}: {e}");
                process::exit(1)
            }),
            path.clone(),
        ),
        None => (synthetic(), "synthetic stream".to_string()),
    };
    let mb = data.len() as f64 / 1e6;
    println!("input: {label} ({mb:.1} MB)");
    println!("runs: {} measured + {} warmup per chunk size, extract: {}", o.runs, o.warmup, o.extract);

    let mut reference: Option<Report> = None;
    for &chunk in &o.chunks {
        let name = chunk.map_or("whole buffer".to_string(), |n| format!("{n}-byte chunks"));
        let mut times = Vec::with_capacity(o.runs);
        let mut outputs = 0;
        for i in 0..o.warmup + o.runs {
            let (t, report, n) = run(&data, chunk, &o.extract);
            match &reference {
                None => reference = Some(report),
                Some(r) if *r != report => {
                    eprintln!("{name}: run {i} produced a different report");
                    process::exit(1);
                }
                Some(_) => {}
            }
            if i >= o.warmup {
                times.push(t.as_secs_f64() * 1e3);
                outputs = n;
            }
        }
        let mut sorted = times.clone();
        sorted.sort_by(f64::total_cmp);
        let mean = times.iter().sum::<f64>() / times.len() as f64;
        let var = times.iter().map(|t| (t - mean).powi(2)).sum::<f64>() / times.len() as f64;
        let rate = |ms: f64| mb / (ms / 1e3);

        println!("\n{name}{}", if o.extract == "none" { String::new() } else { format!(", {outputs} outputs per run") });
        println!("  {:<8} {:>12} {:>12}", "", "time (ms)", "MB/s");
        for (label, ms) in [
            ("min", sorted[0]),
            ("mean", mean),
            ("median", percentile(&sorted, 50.0)),
            ("p95", percentile(&sorted, 95.0)),
            ("p99", percentile(&sorted, 99.0)),
            ("max", sorted[sorted.len() - 1]),
        ] {
            println!("  {label:<8} {ms:>12.3} {:>12.1}", rate(ms));
        }
        println!("  {:<8} {:>12.3}", "stddev", var.sqrt());
    }

    if let Some(r) = reference {
        println!(
            "\nreport: {} packets, {} programs, {} streams",
            r.stats.packets,
            r.programs.len(),
            r.streams().count()
        );
    }
}

/// About 376 MB: a program with H.264 video and async KLV, plus tables.
fn synthetic() -> Vec<u8> {
    fn section(table_id: u8, ext: u16, body: &[u8]) -> Vec<u8> {
        let len = 5 + body.len() + 4;
        let mut s = vec![table_id, 0xB0 | (len >> 8) as u8, len as u8];
        s.extend_from_slice(&ext.to_be_bytes());
        s.extend_from_slice(&[0xC1, 0, 0]);
        s.extend_from_slice(body);
        let crc = mpegts::crc32_mpeg2(&s);
        s.extend_from_slice(&crc.to_be_bytes());
        s
    }
    fn packet(out: &mut Vec<u8>, pid: u16, pusi: bool, cc: u8, payload: &[u8]) {
        let start = out.len();
        out.extend_from_slice(&[0x47, (pusi as u8) << 6 | (pid >> 8) as u8, pid as u8, 0x10 | (cc & 0xF)]);
        out.extend_from_slice(payload);
        out.resize(start + 188, 0xFF);
    }
    let pat = section(0, 1, &[0, 1, 0xF0, 0x00]);
    let pmt = section(
        2,
        1,
        &[0xE1, 0x00, 0xF0, 0x00, 0x1B, 0xE1, 0x00, 0xF0, 0x00, 0x06, 0xE1, 0x01, 0xF0, 0x06, 0x05, 0x04, b'K', b'L', b'V', b'A'],
    );
    let klv = [0, 0, 1, 0xBD, 0, 12, 0x80, 0x80, 5, 0x21, 0, 1, 0, 1, 6, 0x0E, 0x2B, 0x34];
    let pes = [0, 0, 1, 0xE0, 0, 0, 0x80, 0x80, 5, 0x21, 0, 1, 0, 1];
    let mut data = Vec::with_capacity(2_000_000 * 188);
    let mut cc = [0u8; 4];
    for i in 0..2_000_000 {
        let (pid, pusi, slot, payload): (u16, bool, usize, &[u8]) = match i % 200 {
            0 => (0, true, 0, &[]),
            1 => (0x1000, true, 1, &[]),
            2 => (0x101, true, 3, &klv),
            n => (0x100, n % 20 == 3, 2, if n % 20 == 3 { &pes } else { &[] }),
        };
        let table;
        let payload = match slot {
            0 | 1 => {
                table = [&[0u8][..], if slot == 0 { &pat } else { &pmt }].concat();
                &table[..]
            }
            _ => payload,
        };
        packet(&mut data, pid, pusi, cc[slot], payload);
        cc[slot] = (cc[slot] + 1) & 0xF;
    }
    data
}
