//! Structure-aware: the input builds a stream of tables, PES packets, PCRs
//! and corruptions, with subscriptions and tight buffer limits.
#![no_main]

#[path = "../../tests/harness/mod.rs"]
mod harness;

libfuzzer_sys::fuzz_target!(|data: &[u8]| harness::stream(data));
