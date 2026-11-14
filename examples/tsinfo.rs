//! Print what a transport stream file carries.
//!
//!     cargo run --release --example tsinfo -- capture.ts [--live]
//!
//! With `--live`, the file is fed in socket-sized chunks, as a network
//! reader would, and a report is printed whenever the programs or streams
//! change.

use std::{env, fs, process};

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    let Some(path) = args.iter().find(|a| !a.starts_with("--")) else {
        eprintln!("usage: tsinfo <file.ts> [--live]");
        process::exit(2);
    };
    let data = fs::read(path).unwrap_or_else(|e| {
        eprintln!("{path}: {e}");
        process::exit(1);
    });

    if args.iter().any(|a| a == "--live") {
        let mut demux = mpegts::Demuxer::new();
        let mut seen = demux.revision();
        for (i, chunk) in data.chunks(1316).enumerate() {
            demux.push(chunk);
            if demux.revision() != seen {
                seen = demux.revision();
                println!("--- change at byte {} ---\n{}", i * 1316, demux.report());
            }
        }
        demux.finish();
        println!("--- end ---\n{}", demux.report());
    } else {
        print!("{}", mpegts::analyze(&data));
    }
}
