# Changelog

All notable changes to this project are documented here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project uses [semantic versioning](https://semver.org/). Changes to the C ABI are called out separately.

## Unreleased

### Added

- Stream analysis: programs, elementary streams, codec identification (including sync and async KLV), timing, continuity and error counters, for whole buffers and live feeds.
- Extraction of raw packets, PES packets and sections from any stream or table, with bounded, configurable buffers and packet history replay.
- Stable C ABI (`include/mpegts.h`, ABI version 1) with `make install`, pkg-config and CMake support.
- Fuzz harnesses for the stable toolchain and cargo-fuzz.
- Examples: `tsinfo`, `extract`, `json`, `throughput` and the C `klv_dump`.
