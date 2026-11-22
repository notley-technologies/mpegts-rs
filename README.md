# mpegts

A fast MPEG transport stream (ISO/IEC 13818-1) analyser and demultiplexer. Feed it TS bytes and it tells you what the stream carries: programs, and for each elementary stream its kind (video, audio, metadata, subtitle, data) and format (H.264, H.265, AAC, sync/async KLV, ...). It can also hand you the data itself: raw packets, reassembled PES packets, or table sections, from any stream.

It identifies streams but does not decode them, and it never modifies the input. It does no I/O: your application reads files or sockets and hands the bytes over. There is a Rust API and a C API with a stable ABI.

With the optional `mux` feature it also writes transport streams: TS packets, PAT and PMT, PES packets, and KLV metadata in both MISB ST 1402 carriages (see [Writing](#writing)).

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

Sync KLV is carried in metadata access units, with a 5-byte cell header before each KLV unit. By default the payload is delivered as carried; add `.strip_metadata_au()` to a subscription (or set `strip_metadata_au` in C) to receive plain KLV, cells joined in order. A payload that is not valid cells is passed through untouched and marked `damaged`. Async KLV is plain KLV either way.

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

## Writing

The `mux` feature (off by default; a build without it compiles none of this) adds `mpegts::mux`, the primitives for producing a transport stream:

| Type | Writes |
|---|---|
| `TsWriter` | the packets of one PID: continuity counter, payload_unit_start_indicator, adaptation field (PCR, discontinuity, random access), stuffing |
| `ProgramTables` | the PAT and PMTs with descriptors, version numbers and CRCs; every change is checked whole (PID conflicts, 1021-byte section limit) and bumps the versions it touches, or is refused with nothing changed |
| `PesWriter` | PES packets of one stream in a `Carriage`: plain PES (video, audio, private data), synchronous metadata in access unit cells, or asynchronous KLV per SMPTE RP 217 |
| `StreamSpec` | a stream's PMT entry and PES format, with presets such as `StreamSpec::klv_sync(service_id)`, `StreamSpec::klv_async()`, `StreamSpec::h264()` |

```rust
use mpegts::mux::{PesUnit, PesWriter, ProgramTables, StreamSpec};

let video = StreamSpec::h264();
let klv = StreamSpec::klv_sync(0);           // stream_type 0x15, stream_id 0xFC, ST 1402 descriptors
let mut tables = ProgramTables::new(1);
tables.add_program(1, 0x1000, Some(0x100))?;  // program 1, PMT on 0x1000, PCR on the video PID
tables.add_stream(1, 0x100, &video)?;
tables.add_stream(1, 0x101, &klv)?;

let mut video_out = PesWriter::new(0x100, &video)?;
let mut klv_out = PesWriter::new(0x101, &klv)?;
let mut out = Vec::new();                     // or a closure, or a fixed buffer (SliceSink)
tables.write_all(&mut out)?;                  // repeat at least every 100 ms
video_out.write(&PesUnit::new(&frame).with_pts(pts).with_pcr(pcr).with_random_access(), &mut out)?;
klv_out.write(&PesUnit::new(&st0601_packet).with_pts(pts), &mut out)?;
```

- Every writer emits whole 188-byte packets into a `PacketSink`: a closure, a `Vec<u8>`, or a caller's buffer through `SliceSink`. Writing never allocates (configuring tables and specs does), so the writers suit targets without an allocator.
- A write either completes or fails before emitting anything: too small a sink gives `MuxError::BufferTooSmall` with the size needed, and no continuity counter, cell sequence number or version moves.
- Units are checked against their carriage: synchronous KLV needs a PTS and never has a DTS or PCR; asynchronous KLV has no PTS. Synchronous access units above 65,522 bytes are fragmented into one cell per PES packet, each with the access unit's PTS; asynchronous KLV items above 65,532 bytes are split. Non-video PES packets always carry a real `PES_packet_length`.
- `ProgramTables::from_report(&report)` rebuilds the tables the demuxer saw, descriptors verbatim, so a tool can rewrite an input's PMT; a change bumps the input's version by one.
- The values written (descriptor bytes, header flags, cell flags) are the phase 0 decisions in [docs/standards/DECISIONS.md](docs/standards/DECISIONS.md); the code cites the rule behind each one.

`examples/mux.rs` writes a complete synthetic stream (16x16 H.264, synchronous and asynchronous KLV) that players and `ffprobe` read:

```bash
cargo run --features mux --example mux -- out.ts --frames 300
ffprobe -show_streams -show_packets out.ts
```

On the read side, without any feature, `PacketHeader::parse` and `PesHeader::parse` decode single packets and PES headers, `PcrTimeline` gives every packet its time on the program clock (interpolated between PCRs, or extrapolated without holding anything; continuous across clock jumps), which pass-through tools need to pace or insert packets, and `mpegts::nal` reads H.264, H.265 and MPEG-2 video NAL units, SEI messages and user data.

## Injecting KLV

`mux::Injector` (feature `mux`) inserts KLV metadata into an existing transport stream as it passes through, synchronous (stream_type 0x15, a PTS per access unit) or asynchronous (stream_type 0x06, SMPTE RP 217), per MISB ST 1402.2. Every input packet goes out unchanged, in order, except:

- PID 0: the input's own PAT sections, resent byte for byte with the injector's continuity counter;
- the target program's PMT PID: the PMT rewritten with the KLV entry, its version one above the input's, a new CRC; other sections there are copied;
- the KLV PID: the new stream, or, when the program already has synchronous KLV and `ExistingKlv::Keep` applies, the existing stream joined as a new metadata service (its continuity counters and cell sequence numbers renumbered, its access unit bytes untouched; one cell counter per PID, DECISIONS X-8. `CellNumbering::PerAccessUnit`, a workaround for receivers such as MediaMTX that refuse a PES packet whose first cell is not 0, numbers each access unit's cells from 0 and leaves the joined stream's cell headers as they are). `ExistingKlv::Replace` removes the program's KLV instead (a PCR it carries is kept); `ExistingKlv::Fail` refuses.

PAT and PMT copies are added so neither is ever more than `psi_interval_ms` (default 100) apart. Synchronous units are queued by PTS and each goes out at most 990 ms and at least 1 ms before its PTS (the T-STD limit is 1 s), right before the video frame it describes; units that can no longer make it are dropped and counted. Packets are timed on the program clock by PCR interpolation. In `RateMode::Cbr` the output has exactly as many packets as the input: inserted packets replace null packets and the packets the injector consumes, no packet (so no PCR) moves, and a lack of room fails loudly (`OnOverflow::Fail`) or drops units (`OnOverflow::DropOldest`), never silently becoming VBR.

```rust
use mpegts::mux::{Frame, FrameReorder, InjectConfig, Injector, InsertUnit, Units};

let mut injector = Injector::new(InjectConfig::default())?;  // sync KLV, VBR, first program with video
let mut reorder = FrameReorder::new(16);                       // B-frames: decode order -> presentation order
// Called right before each video frame is written; a unit inserted here can
// go out immediately before that frame.
let mut on_frame = |frame: &Frame, units: &mut Units<'_>| {
    reorder.push(*frame);
    while let Some(f) = reorder.pop() {
        let _ = units.insert(InsertUnit::sync(f.pts, st0601_packet_for(f.pts)));
    }
};
let mut out = Vec::new();
for chunk in input_chunks {
    injector.push_with(&chunk, &mut out, &mut on_frame)?;
}
let stats = injector.finish_with(&mut out, &mut on_frame)?;   // frames still held go to the callback too
println!("{} units, {} dropped late", stats.units_inserted, stats.dropped_late);
```

- Input as bytes in any chunking (`push`, which holds each PCR interval until its closing PCR to time it) or as packets with the caller's clock (`push_packet`, for tools that pace output). Until the target program's PMT is known, input is held (`max_held_packets`) so every decision applies from the first packet.
- `insert` queues units outside the callback; `clock()` gives the program clock of the last packet processed, so a scheduler can queue ahead for programs without video. A full queue returns `InjectError::QueueFull` for backpressure; a PTS below the last one sent is refused.
- Refused: scrambled target programs, a PMT that would exceed 1021 bytes, a fixed PID or service id in use, no target within the hold limit. Time base discontinuities drop units that would cross them; the next `Frame` is flagged. `stats()` and `pop_event()` report counters, drops and warnings (for example VBR insertion into a multi-program stream).
- Memory is bounded by the configuration, and the steady state does not allocate. Release-mode throughput on a synthetic 11 Mbit/s stream is about 3 GB/s with `push` and 5 GB/s with `push_packet` (`cargo test --release --features mux --test inject -- --ignored throughput --nocapture`).
- In C: `mpegts_injector_*` in `include/mpegts.h`, with output and frame callbacks.

### Live input: the extrapolating clock

`push` times packets on the program clock. By default (`ClockMode::Interpolate`) it holds each PCR interval until its closing PCR arrives and spreads the packets between the two PCRs, which is exact but adds up to one PCR interval of latency. `InjectConfig::clock = ClockMode::Extrapolate` holds nothing: each packet's clock is extrapolated from the last PCR at the transport rate measured over the previous PCR interval, corrected at every PCR and never going backwards within a time base. With video SEI off, every whole packet pushed is written before `push` returns (once the target program is known), which is what a live injector needs. `PcrTimeline::with_mode` offers the same choice to other pass-through tools.

### SEI user data in the video (MISB ST 0604 timestamps)

`InjectConfig::video_sei` reads, and optionally writes, user data in the target's video, for example the MISB ST 0604 timestamps that anchor KLV time to the frames. The injector stays MISB-agnostic: it reads and writes SEI `user_data_unregistered` messages; the caller decides what goes in them.

- `VideoSei::Off` (default): video bytes are never parsed or changed.
- `VideoSei::Scan`: the frame callback runs once the access unit's prefix (everything before its first VCL NAL unit, or first slice in MPEG-2) has been read, and `Units::user_data()` lists its messages (16-byte identifier, payload without emulation prevention, position) for H.264, H.265 and MPEG-2 video. The output is unchanged.
- `VideoSei::Write` (H.264 stream_type 0x1B and H.265 0x24): `Units::write_user_data(uuid, payload, replace)` writes one message per access unit. A message whose identifier is `uuid` or in `replace` is rewritten in place (same NAL unit and position, payloadSize recoded, emulation prevention run again); otherwise a new SEI NAL unit with a 4-byte start code goes right before the first VCL NAL unit (after the access unit delimiter, parameter sets and existing SEI; before an H.264 prefix NAL unit; H.265 TemporalId copied from the slice), per `docs/standards/st1402-time.md` D-12.

```rust
use mpegts::mux::{Frame, InjectConfig, Injector, Units, VideoSei};

const MISP: [u8; 16] = *b"MISPmicrosectime"; // H.264 ST 0604 identifier
let mut injector = Injector::new(InjectConfig::default().with_video_sei(VideoSei::Write))?;
let mut on_frame = |frame: &Frame, units: &mut Units<'_>| {
    // An ST 0604 message already in the access unit anchors MISP time...
    let anchor = units.user_data().iter().find(|m| m.uuid == MISP).map(|m| m.payload.clone());
    // ...or this access unit gets one (status byte and modified time stamp).
    let _ = units.write_user_data(MISP, &st0604_for(frame.pts, anchor), &[MISP]);
};
```

The video PES is then re-packetized: the new bytes fill the adaptation field stuffing of the PES's packets, `PES_packet_length` is updated when it is not 0, the video PID's continuity counters are renumbered, and PTS, DTS and every PCR stay as they were. In `RateMode::Vbr` bytes that stuffing cannot absorb go into added video packets at the end of the PES; in `RateMode::Cbr` each video PES is held whole and null packets in it carry them, so the packet count stays the input's, and a lack of room fails (`OnOverflow::Fail`) or leaves the access unit unchanged with a warning (`OnOverflow::DropOldest`). Every other PID passes through byte for byte. `mpegts::nal` (no feature needed) has the reading side on its own: Annex B NAL units, emulation prevention, SEI messages, user data, and builders for an SEI NAL unit.

### M2TS

`InjectConfig::record = RecordFormat::M2ts` takes and writes 192-byte M2TS records (a 4-byte TP_extra_header with copy_permission_indicator and arrival_time_stamp, then the packet): `push` frames bytes at 192, `push_record` takes one record with its clock, and the output goes to `PacketSink::record` (a `Vec<u8>` stores whole records; closures can use `RecordFn`). Input records keep their header; a packet the injector adds gets the copy_permission_indicator of the next input record and an arrival_time_stamp interpolated between its neighbours (modulo 2^30); in CBR every record is an input record.

## C API

The header is [include/mpegts.h](include/mpegts.h). Its opening comment states the rules every function follows: status codes, NULL checks, structs that carry their size so fields can be added compatibly, thread use, pointer lifetimes, output buffers and callbacks. No panic crosses into C. [examples/c/klv_dump.c](examples/c/klv_dump.c) shows the usual flow. The write side is there too (`mpegts_program_tables_*`, `mpegts_pes_writer_*`), writing packets into the caller's buffer, and the injector (`mpegts_injector_*`), writing through a callback; `make` builds the library with them.

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
find_package(mpegts 0.2 REQUIRED)
target_link_libraries(app PRIVATE mpegts::mpegts)   # or mpegts::mpegts_static
```

- The shared library's soname carries the ABI version (`libmpegts.so.1`). `mpegts_abi_version()` lets a program check it at run time.
- Static linking needs the system libraries listed by `pkg-config --static --libs mpegts`; the CMake static target adds them. Link with `-Wl,--gc-sections` to drop unused code.
- Two Rust static libraries in one program can clash; use the shared library in that case.
- On Windows, `cargo build --release` produces `mpegts.dll`, its import library `mpegts.dll.lib`, and the static `mpegts.lib`.

## Supported input

- 188-byte TS, 192-byte M2TS and 204-byte (RS parity) packets, auto-detected (the injector takes 188-byte packets or, configured for it, 192-byte M2TS records)
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
cargo run --release --features mux --example mux -- out.ts      # write a synthetic stream
```

`extract` takes `--select klv|video|audio|metadata|psi|all|pid=N|program=N`, `--unit pes|packets|sections`, `--history N` and `--strip-mau`.

## Benchmark

```bash
cargo run --release --example throughput                                  # synthetic stream
cargo run --release --example throughput -- capture.ts --runs 50 --chunk whole,1316 --extract klv
```

Prints min, mean, median, p95, p99 and max per chunk size, in milliseconds and MB/s. `--extract none|klv|pes|packets|sections` measures with extraction running.

## Features

| Feature | Default | Adds |
|---|---|---|
| `capi` | on | the C ABI. Without it the library is compiled with `#![forbid(unsafe_code)]`. |
| `mux` | off | `mpegts::mux`, the write primitives and the KLV injector, and with `capi` their C functions |
| `serde` | off | `Serialize` for the report types |

## Tests

```bash
cargo test --all-features
```

The C tests compile programs against the header and the shared library, check that exported symbols match the header (and that a default build exports none of the write side), write a stream from C and read it back, and run `make install` into a temporary prefix to build the example through pkg-config and CMake, shared and static. Each part is skipped when its tools are missing.

The write side and the injector are also checked against FFmpeg with the KLV patch series (`ffmpeg-klv-fix`) and against TSDuck when you point the tests at them (a wrong path fails the test):

```bash
MPEGTS_FFPROBE=/path/to/ffprobe MPEGTS_FFMPEG=/path/to/ffmpeg cargo test --features mux --test mux --test inject --test video_sei
MPEGTS_TSDUCK=/directory/with/tsp/and/tsanalyze cargo test --features mux --test inject --test video_sei
```

`tests/video_sei.rs` builds H.264, H.265 and MPEG-2 streams with valid parameter sets and slice headers (`tests/harness/es.rs`); FFmpeg's `trace_headers` bitstream filter must show each written SEI message with its identifier and payload, in the right place.

The injector tests run every output through a conformance checker ([tests/harness/check.rs](tests/harness/check.rs)): continuity counters, PSI repetition, the arrival window of every synchronous KLV unit by PCR interpolation, PTS order, PES and cell rules for both carriages, the PMT descriptors, and no PCR on the KLV PID.

## Fuzzing

Six harnesses in [tests/harness](tests/harness/mod.rs) accept any bytes. They check that nothing panics, results do not depend on how input is split into pushes, every buffer stays within its `Config` limit, and the C API returns only documented status codes:

- `demux`: raw bytes.
- `stream`: the bytes drive a generator of tables, PES packets, PCRs and corruption, so mutations explore stream structure.
- `capi`: sequences of C API calls, including invalid arguments and mismatched struct sizes (with `mux`, the write side's calls too).
- `mux`: random tables, streams, units, sink sizes and counter changes for the write primitives; everything accepted must obey the packet rules and read back unchanged through the demuxer.
- `inject`: the injector on generated and mutated synthetic streams (including H.264, H.265 and MPEG-2 video with SEI, and M2TS records) with random configurations, insert schedules and SEI writes; it must stay within its limits, return only documented errors, and pass every packet it does not own through unchanged and in order.
- `nal`: video elementary stream bytes for `mpegts::nal`; positions stay within the input, emulation prevention and SEI rebuilding round-trip.

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
