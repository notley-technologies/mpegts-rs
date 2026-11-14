## What and why

<!-- What this changes and the reason for it. Link the issue if there is one. -->

## Checklist

- [ ] Tests cover the change (a fix includes a test that fails without it)
- [ ] `cargo test --all-features` passes
- [ ] No new warnings (`RUSTFLAGS="-D warnings" cargo build --all-targets --all-features`)
- [ ] C ABI changes only add functions, constants or trailing struct fields, and `include/mpegts.h` is updated
- [ ] CHANGELOG.md updated for user-visible changes
- [ ] Throughput numbers included for changes to the demuxer hot path
