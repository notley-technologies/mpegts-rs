//! The KLV injector: random input streams and insert schedules; nothing may
//! panic, memory stays within the limits, packets the injector does not own
//! pass through unchanged.
#![no_main]

#[path = "../../tests/harness/mod.rs"]
mod harness;

libfuzzer_sys::fuzz_target!(|data: &[u8]| harness::inject(data));
