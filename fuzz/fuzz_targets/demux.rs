//! Raw bytes: sync hunting, framing, and everything behind it. Works best
//! with real captures in the seed corpus.
#![no_main]

#[path = "../../tests/harness/mod.rs"]
mod harness;

libfuzzer_sys::fuzz_target!(|data: &[u8]| harness::demux(data));
