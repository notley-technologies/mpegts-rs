//! Fuzzing on the stable toolchain. Replays the saved inputs in
//! `tests/fuzz_regressions/<target>/`, then runs each harness on randomly
//! mutated inputs. Coverage-guided fuzzing of the same harnesses is in
//! `fuzz/` (cargo-fuzz).
//!
//! Longer runs:
//!
//!     MPEGTS_FUZZ_ITERATIONS=200000 cargo test --release --test fuzz
//!
//! `MPEGTS_FUZZ_SEED=<n>` picks a different random sequence. A failing
//! input is saved under `target/fuzz-failures/`; once fixed, copy it into
//! `tests/fuzz_regressions/<target>/` so it is replayed from then on.
mod harness;

use harness::common::*;
use mpegts::{Demuxer, Report, Select, Subscription, Unit};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};

fn iterations(default: usize) -> usize {
    std::env::var("MPEGTS_FUZZ_ITERATIONS").ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

fn seed() -> u64 {
    std::env::var("MPEGTS_FUZZ_SEED").ok().and_then(|v| v.parse().ok()).unwrap_or(0x5EED_CAFE) | 1
}

fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

/// Saved inputs for `target`, in name order.
fn regressions(target: &str) -> Vec<(PathBuf, Vec<u8>)> {
    let dir = root().join("tests/fuzz_regressions").join(target);
    let mut files: Vec<PathBuf> = match std::fs::read_dir(&dir) {
        Ok(entries) => entries.map(|e| e.unwrap().path()).filter(|p| p.is_file()).collect(),
        Err(_) => return Vec::new(),
    };
    files.sort();
    files.into_iter().map(|p| (p.clone(), std::fs::read(&p).unwrap())).collect()
}

fn random_bytes(rng: &mut Rng, len: usize) -> Vec<u8> {
    (0..len).map(|_| rng.next() as u8).collect()
}

/// A few byte-level edits, the kind a mutation fuzzer makes.
fn mutate(rng: &mut Rng, v: &mut Vec<u8>) {
    for _ in 0..1 + rng.below(8) {
        let len = v.len();
        match rng.below(8) {
            0 if len > 0 => v[rng.below(len)] ^= 1 << rng.below(8),
            1 if len > 0 => v[rng.below(len)] = [0x00, 0xFF, 0x47, 0x80, 0x7F, 0x01, 0x10][rng.below(7)],
            2 => {
                let at = rng.below(len + 1);
                let n = 1 + rng.below(16);
                let junk = random_bytes(rng, n);
                v.splice(at..at, junk);
            }
            3 if len > 0 => {
                let at = rng.below(len);
                let n = 1 + rng.below((len - at).min(400));
                v.drain(at..at + n);
            }
            4 if len > 1 => {
                let from = rng.below(len);
                let n = 1 + rng.below((len - from).min(400));
                let at = rng.below(len + 1);
                let part = v[from..from + n].to_vec();
                v.splice(at..at, part);
            }
            5 => {
                let n = 1 + rng.below(64);
                let junk = random_bytes(rng, n);
                v.extend(junk);
            }
            6 if len > 0 => v.truncate(rng.below(len)),
            _ => {}
        }
    }
}

fn fuzz(target: &str, harness: fn(&[u8]), seeds: Vec<Vec<u8>>, default_iterations: usize) {
    for (path, data) in regressions(target) {
        if catch_unwind(AssertUnwindSafe(|| harness(&data))).is_err() {
            panic!("{target}: saved input {} fails again", path.display());
        }
    }
    let mut rng = Rng(seed() ^ target.len() as u64);
    let mut corpus = seeds;
    for i in 0..iterations(default_iterations) {
        let mut input = if rng.below(8) == 0 {
            let n = rng.below(3000);
            random_bytes(&mut rng, n)
        } else {
            corpus[rng.below(corpus.len())].clone()
        };
        mutate(&mut rng, &mut input);
        if catch_unwind(AssertUnwindSafe(|| harness(&input))).is_err() {
            let dir = root().join("target/fuzz-failures");
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join(format!("{target}-{i}.bin"));
            std::fs::write(&path, &input).unwrap();
            panic!("{target}: iteration {i} failed; input saved to {}", path.display());
        }
        // Keep some mutants, so edits stack up over the run.
        if i % 8 == 0 && corpus.len() < 512 {
            corpus.push(input);
        }
    }
}

/// Valid streams to mutate: tables, video, audio, both KLV forms, PCR, in
/// 188- and 192-byte framing. The first byte is the harness's control byte.
fn demux_seeds() -> Vec<Vec<u8>> {
    let mut ts = Ts::new();
    ts.section(0, &pat(1, 0, &[(1, 0x1000), (2, 0x1001)]));
    ts.section(
        0x1000,
        &pmt(1, 0, 0x100, &[], &[(H264, 0x100, vec![]), (AAC, 0x101, language(b"eng")), (PRIVATE, 0x102, registration(b"KLVA"))]),
    );
    ts.section(0x1001, &pmt(2, 0, 0x200, &[], &[(METADATA, 0x200, metadata_descriptor(b"KLVA"))]));
    for i in 0..6u64 {
        ts.pcr(0x100, i * 2_700_000);
        ts.pes(0x100, 0xE0, Some(i * 3003), &[0x55; 300]);
        ts.pes(0x101, 0xC0, Some(i * 1920), &[0xFF, 0xF1, 0x50, 0x80, 0x02, 0x1F, 0xFC]);
        ts.pes(0x102, 0xBD, Some(i * 9000), &KLV_KEY);
        let mut sync = vec![0x00, 0x00, 0x00, 0x00, 0x00];
        sync.extend_from_slice(&KLV_KEY);
        ts.pes(0x200, 0xFC, Some(i * 9000), &sync);
    }
    let plain = ts.out.clone();
    let mut m2ts = Vec::new();
    for (i, p) in plain.chunks(188).enumerate() {
        m2ts.extend_from_slice(&(i as u32).to_be_bytes());
        m2ts.extend_from_slice(p);
    }
    let with = |control: u8, body: &[u8]| [&[control][..], body].concat();
    vec![with(0x00, &plain), with(0x21, &plain), with(0x62, &m2ts), with(0x40, &plain[..188 * 5])]
}

fn random_seeds(lens: &[usize]) -> Vec<Vec<u8>> {
    let mut rng = Rng(seed());
    lens.iter().map(|&n| random_bytes(&mut rng, n)).collect()
}

#[test]
fn fuzz_demux() {
    fuzz("demux", harness::demux, demux_seeds(), 600);
}

#[test]
fn fuzz_stream() {
    fuzz("stream", harness::stream, random_seeds(&[64, 256, 1024, 4096]), 800);
}

#[cfg(feature = "capi")]
#[test]
fn fuzz_capi() {
    fuzz("capi", harness::capi, random_seeds(&[64, 512, 2048]), 400);
}

#[cfg(feature = "mux")]
#[test]
fn fuzz_mux() {
    fuzz("mux", harness::mux, random_seeds(&[16, 128, 1024, 4096]), 300);
}

/// The generator is only useful if what it builds gets past sync and
/// table parsing. Check that most inputs produce streams and outputs.
#[test]
fn generator_reaches_deep_paths() {
    let mut rng = Rng(seed());
    let (mut with_streams, mut with_pes, mut with_sections, runs) = (0, 0, 0, 300);
    for _ in 0..runs {
        let n = 200 + rng.below(3000);
        let data = random_bytes(&mut rng, n);
        let ts = harness::generate(&mut harness::Input::new(&data));
        let mut d = Demuxer::new();
        d.subscribe(Subscription::new(Select::All, Unit::Pes)).unwrap();
        d.subscribe(Subscription::new(Select::Psi, Unit::Sections)).unwrap();
        let mut declared = false;
        for chunk in ts.chunks(188 * 4) {
            d.push(chunk);
            let report: Report = d.report();
            declared |= report.streams().next().is_some();
        }
        d.finish();
        with_streams += usize::from(declared);
        let units: Vec<Unit> = d.outputs().map(|o| o.unit).collect();
        with_pes += usize::from(units.contains(&Unit::Pes));
        with_sections += usize::from(units.contains(&Unit::Sections));
    }
    assert!(with_streams * 2 > runs, "only {with_streams}/{runs} generated streams declared a stream");
    assert!(with_pes * 4 > runs, "only {with_pes}/{runs} produced a PES");
    assert!(with_sections * 4 > runs * 3, "only {with_sections}/{runs} produced a section");
}

/// The write harness turns most random inputs into tables and units.
#[cfg(feature = "mux")]
#[test]
fn mux_harness_reaches_deep_paths() {
    let mut rng = Rng(seed());
    let (mut writing, mut splitting, mut tables, runs) = (0, 0, 0, 300);
    for _ in 0..runs {
        let n = 50 + rng.below(3000);
        let run = harness::mux_run(&random_bytes(&mut rng, n));
        writing += usize::from(run.units > 0);
        splitting += usize::from(run.split > 0);
        tables += usize::from(run.table_changes > 0);
    }
    assert!(writing * 2 > runs, "only {writing}/{runs} runs wrote a unit");
    assert!(splitting * 20 > runs, "only {splitting}/{runs} runs split a unit");
    assert!(tables * 2 > runs, "only {tables}/{runs} runs changed the tables");
}
