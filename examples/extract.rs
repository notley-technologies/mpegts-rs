//! Extract packets, PES packets or sections from a transport stream, the
//! way a live consumer would: the file is fed in socket-sized chunks and
//! outputs are collected as they appear.
//!
//!     cargo run --release --example extract -- FILE [options]
//!
//! --select SPEC   klv (default), video, audio, metadata, psi, all,
//!                 pid=N, program=N  (N decimal or 0x hex)
//! --unit UNIT     pes (default), packets, sections
//! --out PATH      write the extracted bytes to PATH, or `-` for stdout
//!                 (PES: the payload after the PES header; otherwise the
//!                 whole packet or section). Summary goes to stderr.
//! --history N     keep N packets and replay them into the subscription
//! --verbose       one line per output on stderr
//!
//! Pipe KLV into another tool:
//!     cargo run --release --example extract -- capture.ts --out - | klv-parser

use mpegts::{Codec, Config, Demuxer, Select, StreamKind, Subscription, Unit};
use std::io::Write;
use std::{env, fs, io, process};

fn usage(msg: &str) -> ! {
    eprintln!("{msg}\nusage: extract FILE [--select klv|video|audio|metadata|psi|all|pid=N|program=N] [--unit pes|packets|sections] [--out PATH|-] [--history N] [--verbose]");
    process::exit(2);
}

fn number(s: &str) -> u16 {
    let parsed = match s.strip_prefix("0x") {
        Some(hex) => u16::from_str_radix(hex, 16),
        None => s.parse(),
    };
    parsed.unwrap_or_else(|_| usage(&format!("bad number {s}")))
}

fn main() {
    let mut file = None;
    let mut select = "klv".to_string();
    let mut unit = Unit::Pes;
    let mut out_path = None;
    let mut history = 0;
    let mut verbose = false;
    let mut args = env::args().skip(1);
    while let Some(a) = args.next() {
        let mut value = || args.next().unwrap_or_else(|| usage("missing value"));
        match a.as_str() {
            "--select" => select = value(),
            "--unit" => {
                unit = match value().as_str() {
                    "pes" => Unit::Pes,
                    "packets" => Unit::Packets,
                    "sections" => Unit::Sections,
                    other => usage(&format!("unknown unit {other}")),
                }
            }
            "--out" => out_path = Some(value()),
            "--history" => history = value().parse().unwrap_or_else(|_| usage("--history needs a number")),
            "--verbose" => verbose = true,
            f if !f.starts_with('-') && file.is_none() => file = Some(f.to_string()),
            other => usage(&format!("unknown argument {other}")),
        }
    }
    let Some(file) = file else { usage("no input file") };
    let data = fs::read(&file).unwrap_or_else(|e| {
        eprintln!("{file}: {e}");
        process::exit(1)
    });

    let selects: Vec<Select> = match select.as_str() {
        "klv" => vec![Select::Codec(Codec::KlvAsync), Select::Codec(Codec::KlvSync)],
        "video" => vec![Select::Kind(StreamKind::Video)],
        "audio" => vec![Select::Kind(StreamKind::Audio)],
        "metadata" => vec![Select::Kind(StreamKind::Metadata)],
        "psi" => vec![Select::Psi],
        "all" => vec![Select::All],
        s => match s.split_once('=') {
            Some(("pid", n)) => vec![Select::Pid(number(n))],
            Some(("program", n)) => vec![Select::Program(number(n))],
            _ => usage(&format!("unknown selection {s}")),
        },
    };

    let mut config = Config::default();
    config.history_packets = history;
    let mut demux = Demuxer::with_config(config).unwrap_or_else(|e| usage(&e.to_string()));
    for s in selects {
        let mut sub = Subscription::new(s, unit);
        sub.replay = history > 0;
        demux.subscribe(sub).unwrap_or_else(|e| usage(&e.to_string()));
    }

    let mut sink: Option<Box<dyn Write>> = match out_path.as_deref() {
        None => None,
        Some("-") => Some(Box::new(io::BufWriter::new(io::stdout().lock()))),
        Some(p) => Some(Box::new(io::BufWriter::new(fs::File::create(p).unwrap_or_else(|e| {
            eprintln!("{p}: {e}");
            process::exit(1)
        })))),
    };
    let (mut count, mut bytes) = (0u64, 0u64);
    let mut collect = |demux: &mut Demuxer| {
        while let Some(o) = demux.pop_output() {
            count += 1;
            bytes += o.payload().len() as u64;
            if verbose {
                eprintln!(
                    "{:?} pid=0x{:04x} packet={} len={} pts={:?} stream_id={:?} table_id={:?} crc_ok={:?}{}{}",
                    o.unit,
                    o.pid,
                    o.packet,
                    o.payload().len(),
                    o.pts,
                    o.stream_id,
                    o.table_id,
                    o.crc_ok,
                    if o.damaged { " DAMAGED" } else { "" },
                    if o.truncated { " TRUNCATED" } else { "" },
                );
            }
            if let Some(w) = sink.as_mut() {
                if let Err(e) = w.write_all(o.payload()) {
                    eprintln!("write failed: {e}");
                    process::exit(1);
                }
            }
            demux.recycle(o);
        }
    };
    for chunk in data.chunks(1316) {
        demux.push(chunk);
        collect(&mut demux);
    }
    demux.finish();
    collect(&mut demux);
    if let Some(mut w) = sink {
        let _ = w.flush();
    }

    let s = demux.output_stats();
    eprintln!(
        "{count} outputs, {bytes} bytes; damaged {}, truncated {}, dropped {}",
        s.damaged, s.truncated, s.dropped
    );
}
