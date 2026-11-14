# Contributing

Thanks for taking the time to help. Bug reports, fixes, new stream identifications and documentation improvements are all welcome.

## Reporting problems

Open an issue using the bug report template. The most useful reports include:

- the version or commit you are using,
- what the report said and what you expected,
- if you can share it, a short capture that shows the problem. A few hundred kilobytes is usually enough; cut one with `head -c 500000 capture.ts > sample.ts`. Please do not attach captures you are not allowed to share.

Security issues are different: see [SECURITY.md](SECURITY.md).

## Development

You need a stable Rust toolchain (the minimum supported version is in `Cargo.toml`). For the C tests you also need a C compiler, `make`, `pkg-config` and optionally `cmake`; tests that need missing tools skip themselves.

```bash
cargo test --all-features
```

This runs the unit and integration tests, the C ABI and install tests, and a short fuzzing pass. CI runs the same on Linux, macOS and Windows, plus a build with warnings as errors, a minimum Rust version check and two minutes of coverage-guided fuzzing per target.

Before opening a pull request:

```bash
RUSTFLAGS="-D warnings" cargo build --all-targets --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --all-features
```

For changes to parsing or extraction, a longer fuzz run is worth it:

```bash
MPEGTS_FUZZ_ITERATIONS=200000 cargo test --release --test fuzz
```

If the fuzzer finds a failing input, fix the bug and add the input to `tests/fuzz_regressions/<target>/` so it is replayed from then on.

## Guidelines

- **Tests.** Fixes come with a test that fails without the fix. Synthetic streams are built with the helpers in `tests/common/mod.rs`; prefer those over binary files.
- **No I/O, no mutation.** The library never reads files or sockets and never changes the stream. Keep it that way.
- **Bounded memory.** Anything that buffers data must have a limit in `Config` or a fixed cap. Hostile input must not be able to grow memory without bound.
- **Hot path.** Per-packet work should stay constant time. Run the throughput example before and after changes to the demuxer and include the numbers in the pull request.
- **C ABI.** Within an `MPEGTS_ABI_VERSION`, only add: new functions, new constants, and new fields at the end of structs. Never change or remove existing ones, and never reuse a codec code. Update `include/mpegts.h` alongside `src/capi.rs`; the tests check that the two agree.
- **Standards.** When identifying a new stream type or descriptor, cite the document and section in a comment.
- **Style.** Match the surrounding code. Comments explain why, not what. Use hyphens, not em dashes.

## Pull requests

Keep pull requests focused on one change and describe what it does and why. Add an entry under "Unreleased" in [CHANGELOG.md](CHANGELOG.md) for anything user-visible.

## License

By contributing, you agree that your contributions are licensed under the [Apache License 2.0](LICENSE), as described in section 5 of the license.
