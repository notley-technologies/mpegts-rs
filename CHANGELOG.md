# Changelog

All notable changes to this project are documented here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project uses [semantic versioning](https://semver.org/). Changes to the C ABI are called out separately.

## Unreleased (0.2.0)

### Added

- `mux` feature (opt-in; a default build compiles none of it): transport stream write primitives in `mpegts::mux`, writing whole packets into caller-provided sinks without allocating.
  - `TsWriter`: the packets of one PID, with continuity counter, payload_unit_start_indicator, adaptation fields (PCR, discontinuity, random access, priority), adaptation field stuffing for PES and 0xFF fill for sections, adaptation-field-only packets, `NULL_PACKET`.
  - `ProgramTables`: PAT (with the network PID) and PMTs for any number of programs, descriptors, version numbers bumped modulo 32 on every change, CRC-32/MPEG-2, the 1021-byte section_length limit, PID conflict checks, transactional changes; `ProgramTables::from_report` rebuilds an input's tables from a demuxer `Report`. Allocation-free section builders `write_pat_section`, `write_pmt_section`, `write_descriptor`.
  - `PesWriter` and `PesUnit`: PES packets with PTS/DTS (33-bit wrap, marker bits), real `PES_packet_length` for everything but oversized video, splitting of oversized non-video units, and three carriages (`Carriage::Pes`, `Carriage::MetadataCells`, `Carriage::AsyncKlv`).
  - Metadata access unit cells (`CellHeader`, `CellFragment`): per-PID sequence numbers, fragmentation of access units above 65,522 bytes over several PES packets with the access unit's PTS, random_access_indicator on complete and first cells.
  - KLV carriage presets per MISB ST 1402.2 and SMPTE RP 217 and `docs/standards/DECISIONS.md`: `StreamSpec::klv_sync` / `klv_sync_with` (stream_type 0x15, stream_id 0xFC, metadata_descriptor and metadata_STD_descriptor, optional registration descriptor, several services per stream) and `StreamSpec::klv_async` (stream_type 0x06, stream_id 0xBD, "KLVA" registration, one KLV item per PES).
  - `MuxError`, `PacketSink`, `SliceSink`.
  - `Injector`: pass-through KLV injection into an existing transport stream, synchronous or asynchronous per MISB ST 1402.2. Only PID 0 (the input's PAT, resent unchanged), the target PMT PID (PMT rewritten with the KLV entry, version + 1, new CRC) and the KLV PID change; PAT and PMT copies are added so neither is more than `psi_interval_ms` apart. Synchronous units are queued in PTS order and sent within the T-STD window (990 ms to 1 ms before their PTS by default), right before their video frame; late units and units across a time base discontinuity are dropped and counted. `ExistingKlv::Keep` joins an existing synchronous stream as a new metadata service (continuity counters and cell sequence numbers renumbered, access unit bytes untouched), `Replace` removes the program's KLV (keeping a PCR it carries), `Fail` refuses. `RateMode::Vbr` adds packets; `RateMode::Cbr` keeps the packet count and every packet's position, filling null and consumed packets, and fails or drops units (`OnOverflow`) when there is no room. Byte input with an internal `PcrTimeline` (`push`) or pre-timed packets (`push_packet`), a frame callback that runs right before each video frame (`push_with`, `finish_with`, `Units`), `insert`, `end_input_with` (process what is held, then insert the last units), `finish`, `target`, `clock`, `stats` (`InjectStats`), `pop_event` (`InjectEvent`), `InjectConfig`, `InjectError`. Bounded memory, no allocation in steady state.
  - `FrameReorder`: video frames from decode order back to presentation order, for per-frame metadata with B-frames.
- Read-side utilities (no feature needed): `PacketHeader` (TS header and adaptation field), `PesHeader` (PES header fields and timestamps), `PcrTimeline` (program clock time of every packet, interpolated between PCRs), `pts_delta`, `pcr_delta`, `PTS_WRAP`, `PCR_WRAP`, `NULL_PID`.
- `capi` feature, on by default, for the C ABI; without it the library is compiled with `#![forbid(unsafe_code)]`.
- Example `mux`, which writes a synthetic stream with H.264 video and both KLV carriages.
- Fuzz harness and cargo-fuzz target `mux`; the `capi` harness also drives the write side.
- Fuzz harness and cargo-fuzz target `inject`; a conformance checker for injected streams in the tests (`tests/harness/check.rs`), and tests against the patched FFmpeg (`MPEGTS_FFPROBE`, `MPEGTS_FFMPEG`) and TSDuck (`MPEGTS_TSDUCK`, run in CI).

### C ABI

Additive; `MPEGTS_ABI_VERSION` stays 1. The library built by `make` includes the write side (the `mux` feature).

- `MPEGTS_ERR_BUFFER` and the output buffer convention: `(out, capacity, written)`, with `*written` set to the size needed on success and on `MPEGTS_ERR_BUFFER`.
- `mpegts_stream_spec`, `mpegts_stream_spec_pes`, `mpegts_stream_spec_klv_sync`, `mpegts_stream_spec_klv_async`, `MPEGTS_CARRIAGE_*`, `MPEGTS_SPEC_*`.
- `mpegts_program_tables` and its functions: `_new`, `_from_report`, `_free`, `_error`, `_add_program`, `_remove_program`, `_set_pcr_pid`, `_set_network_pid`, `_add_program_descriptor`, `_add_stream`, `_add_stream_descriptor`, `_remove_stream`, `_pat_version`, `_pmt_version`, `_set_pat_version`, `_set_pmt_version`, `_pat_section`, `_pmt_section`, `_write_pat`, `_write_pmt`, `_write_all`, `_continuity_counter`, `_set_continuity_counter`.
- `mpegts_pes_unit`, `MPEGTS_PES_*`, `mpegts_pes_writer` and its functions: `_new`, `_free`, `_error`, `_write`, `_write_adaptation_only`, `_continuity_counter`, `_set_continuity_counter`, `_mark_discontinuity`, `_sequence_number`, `_set_sequence_number`.
- `mpegts_null_packet`.
- The injector: `mpegts_inject_config`, `mpegts_inject_config_default`, `mpegts_inject_config_check`, `mpegts_injector_new`, `_free`, `_error`, `_push`, `_push_packet`, `_insert`, `_end_input`, `_finish`, `_stats`, `_target`, `_clock`, `_pop_event`; inside the frame callback `mpegts_inject_units_insert`, `mpegts_inject_units_clock`, `mpegts_inject_units_target`; the structs `mpegts_inject_unit`, `mpegts_inject_frame`, `mpegts_inject_target`, `mpegts_inject_stats`, `mpegts_inject_event`; the callback types `mpegts_packets_fn` and `mpegts_frame_fn`; constants `MPEGTS_INJECT_*`, `MPEGTS_EXISTING_KLV_*`, `MPEGTS_RATE_*`, `MPEGTS_FRAME_*`, `MPEGTS_TARGET_*`, `MPEGTS_EVENT_*`, `MPEGTS_DROP_*`, `MPEGTS_WARN_*`.
- Status codes `MPEGTS_ERR_FULL` (a queue is full; try again) and `MPEGTS_ERR_STREAM` (the input cannot be processed as configured), and the callback rule in the header's opening comment.

### Changed

- `mpegts_status_string(MPEGTS_ERR_POISONED)` reads "unusable after an internal error", since program tables and PES writers can be poisoned too.
- The CMake package now requires `find_package(mpegts 0.2)`.

## 0.1.0

### Added

- Stream analysis: programs, elementary streams, codec identification (including sync and async KLV), timing, continuity and error counters, for whole buffers and live feeds.
- Extraction of raw packets, PES packets and sections from any stream or table, with bounded, configurable buffers and packet history replay.
- Stable C ABI (`include/mpegts.h`, ABI version 1) with `make install`, pkg-config and CMake support.
- `Subscription::strip_metadata_au` (C: `mpegts_subscription.strip_metadata_au`) to deliver sync KLV without its metadata access unit cell headers.
- Fuzz harnesses for the stable toolchain and cargo-fuzz.
- Examples: `tsinfo`, `extract`, `json`, `throughput` and the C `klv_dump`.
