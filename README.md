# mpegts

A fast MPEG transport stream (ISO/IEC 13818-1) analyser and demultiplexer. Feed it TS bytes and it tells you what the stream carries: programs, and for each elementary stream its kind (video, audio, metadata, subtitle, data) and format (H.264, H.265, AAC, sync/async KLV, ...). It can also hand you the data itself: raw packets, reassembled PES packets, or table sections, from any stream.

It identifies streams but does not decode them, and it never modifies the input. It does no I/O: your application reads files or sockets and hands the bytes over. There is a Rust API and a C API with a stable ABI.

## Usage

Whole buffer:

```rust
let report = mpegts::analyze(&bytes);

for s in report.video() {
    println!("video on PID {:#x}: {}", s.pid, s.codec);
}
if report.klv().next().is_some() {
    println!("stream carries KLV metadata");
}
```

Live feed, pushed in chunks of any size. The report always reflects the current PAT/PMT, so streams that are added, removed or changed show up as they happen:

```rust
let mut demux = mpegts::Demuxer::new();
let mut seen = demux.revision();
loop {
    demux.push(&socket_bytes());
    if demux.revision() != seen {       // cheap; changes only when structure does
        seen = demux.revision();
        let report = demux.report();    // snapshot
        println!("{report}");
    }
}
```

Use one `Demuxer` per feed. Call `finish()` at end of input so trailing packets are accepted.

## Querying a report

| Call | Returns |
|---|---|
| `report.streams()` | every stream in every program |
| `report.video()` / `audio()` / `metadata()` / `streams_of(kind)` | streams of one kind |
| `report.klv()` | sync and async KLV streams |
| `report.count(kind)`, `report.has(codec)` | counts and presence checks |
| `report.stream(pid)`, `report.program(number)` | lookups |
| `report.active_streams(window)` | streams that sent a packet in the last `window` packets |
| `report.duration()`, `stream.duration()` | time spans from PCR / PTS; handle wraparound and splices |
| `report.bitrate()`, `stream.bitrate` | average bits/s including TS overhead |

Each `Stream` exposes its program and PID, raw `stream_type`, `codec`, `kind()`, the `basis` of the identification (stream type, descriptor, or payload inspection), language, registration and metadata format identifiers, all raw descriptors, PTS/DTS, packet and CC-error counters, and whether it is scrambled right now. `Report` also carries feed stats (packet size, sync losses, bytes skipped, CRC errors), the CAT's CA systems, and per-PID counters. With the `serde` feature, every report type implements `Serialize`.

## Extracting data

Subscribe to what you want, push bytes, collect outputs:

```rust
use mpegts::{Codec, Demuxer, Select, Subscription, Unit};

let mut demux = Demuxer::new();
demux.subscribe(Subscription::new(Select::Codec(Codec::KlvSync), Unit::Pes))?;
demux.subscribe(Subscription::new(Select::Psi, Unit::Sections))?;

demux.push(&bytes);
for out in demux.outputs() {
    match out.unit {
        Unit::Pes => klv_parser(out.payload(), out.pts),   // KLV bytes after the PES header
        _ => table_analyser(&out.data, out.crc_ok),        // whole section
    }
}
```

| `Select` | Follows |
|---|---|
| `Pid(n)` | one PID, whatever it carries (PAT = 0, CAT = 1, a PMT, SDT, ...) |
| `Program(n)` | a program's PMT, PCR and elementary stream PIDs |
| `Codec(c)`, `Kind(k)` | every stream with that format or kind, in any program |
| `Psi` | PAT, CAT, PMTs, NIT, SI PIDs 0x02-0x1F, ATSC PSIP |
| `All` | every PID |

Units are `Packets` (raw 188-byte packets, damaged ones included), `Pes` (reassembled, with PTS/DTS) and `Sections` (with a CRC check). Selections follow the tables as they change, so a KLV stream that appears mid-feed is picked up. Outputs flag `damaged` (data lost inside) and `truncated` (hit a size limit).

Sync KLV is carried in metadata access units, with a 5-byte cell header before each KLV unit. By default the payload is delivered as carried; add `.strip_metadata_au()` to a subscription (or set `strip_metadata_au` in C) to receive plain KLV, cells joined in order. Async KLV is plain KLV either way.

## Configuration

Every buffer is bounded and configurable through `Config`:

```rust
let mut config = mpegts::Config::default();
config.history_packets = 20_000;   // keep ~3.7 MB of recent packets...
let mut demux = mpegts::Demuxer::with_config(config)?;
// ...so a late subscription can replay them and miss nothing
demux.subscribe(Subscription::new(Select::Codec(Codec::KlvAsync), Unit::Pes).with_replay())?;
```

| Field | Default | Purpose |
|---|---|---|
| `history_packets` | 0 | recent packets kept for replay |
| `queue_max_items`, `queue_max_bytes`, `overflow` | 65,536, 64 MiB, drop newest | output queue bounds and policy |
| `max_pes_bytes`, `max_reassembly_bytes` | 8 MiB, 64 MiB | PES size limit, total in-progress PES memory |
| `max_streams` | 4096 | stream entries tracked; extras counted in `stats.streams_dropped` |
| `probe_pes` | 4 | PES packets inspected to identify vaguely declared streams |
| `sync_confirmations` | 2 | evidence required before trusting packet alignment |

`output_stats()` reports queued, dropped, damaged and truncated outputs and current memory use.

## C API

The header is [include/mpegts.h](include/mpegts.h). Its opening comment states the rules every function follows: status codes, NULL checks, structs that carry their size so fields can be added compatibly, thread use and pointer lifetimes. No panic crosses into C. [examples/c/klv_dump.c](examples/c/klv_dump.c) shows the usual flow.

Install the shared and static libraries, the header, a pkg-config file and a CMake package:

```bash
make
sudo make install      # PREFIX=/usr/local by default; DESTDIR, LIBDIR, INCLUDEDIR honoured
```

Then build with pkg-config or CMake:

```bash
cc app.c $(pkg-config --cflags --libs mpegts)
```

```cmake
find_package(mpegts 0.1 REQUIRED)
target_link_libraries(app PRIVATE mpegts::mpegts)   # or mpegts::mpegts_static
```

- The shared library's soname carries the ABI version (`libmpegts.so.1`). `mpegts_abi_version()` lets a program check it at run time.
- Static linking needs the system libraries listed by `pkg-config --static --libs mpegts`; the CMake static target adds them. Link with `-Wl,--gc-sections` to drop unused code.
- Two Rust static libraries in one program can clash; use the shared library in that case.
- On Windows, `cargo build --release` produces `mpegts.dll`, its import library `mpegts.dll.lib`, and the static `mpegts.lib`.

## Supported input

- 188-byte TS, 192-byte M2TS and 204-byte (RS parity) packets, auto-detected
- Resynchronisation after corrupt or missing bytes
- Identification by stream type, by DVB/registration/metadata descriptors, and by payload inspection when a PMT is vague (e.g. undeclared KLV)
- KLV per MISB ST 1402: sync (stream type 0x15 + `KLVA` metadata descriptor) and async (0x06 + `KLVA` registration, SMPTE RP 217)
- Hostile or corrupt input: oversized sections are rejected and all memory is bounded by `Config`

## Examples

```bash
cargo run --release --example tsinfo -- capture.ts            # table of what is inside
cargo run --release --example tsinfo -- capture.ts --live     # report on each change
cargo run --release --example extract -- capture.ts --out -   # KLV payloads to stdout
cargo run --release --features serde --example json -- capture.ts
```

`extract` takes `--select klv|video|audio|metadata|psi|all|pid=N|program=N`, `--unit pes|packets|sections`, `--history N` and `--strip-mau`.

## Benchmark

```bash
cargo run --release --example throughput                                  # synthetic stream
cargo run --release --example throughput -- capture.ts --runs 50 --chunk whole,1316 --extract klv
```

Prints min, mean, median, p95, p99 and max per chunk size, in milliseconds and MB/s. `--extract none|klv|pes|packets|sections` measures with extraction running.

## Tests

```bash
cargo test --all-features
```

The C tests compile programs against the header and the shared library, check that exported symbols match the header, and run `make install` into a temporary prefix to build the example through pkg-config and CMake, shared and static. Each part is skipped when its tools are missing.

## Fuzzing

Three harnesses in [tests/harness](tests/harness/mod.rs) accept any bytes. They check that nothing panics, results do not depend on how input is split into pushes, every buffer stays within its `Config` limit, and the C API returns only documented status codes:

- `demux`: raw bytes.
- `stream`: the bytes drive a generator of tables, PES packets, PCRs and corruption, so mutations explore stream structure.
- `capi`: sequences of C API calls, including invalid arguments and mismatched struct sizes.

`cargo test` runs them over a few thousand mutated inputs on the stable toolchain. For longer runs, or coverage-guided fuzzing with [cargo-fuzz](https://github.com/rust-fuzz/cargo-fuzz):

```bash
MPEGTS_FUZZ_ITERATIONS=200000 cargo test --release --test fuzz
cargo +nightly fuzz run stream
cargo +nightly fuzz run demux fuzz/corpus/demux my-captures/   # seed with real .ts files
```

The stable runner saves a failing input under `target/fuzz-failures/`. Once it is fixed, add the input to `tests/fuzz_regressions/<target>/` so every test run replays it.

## Contributing

Issues and pull requests are welcome; see [CONTRIBUTING.md](CONTRIBUTING.md). Report security problems privately as described in [SECURITY.md](SECURITY.md).

## License

Copyright 2026 Notley Technologies and Noah Davis.

Licensed under the [Apache License, Version 2.0](LICENSE).
