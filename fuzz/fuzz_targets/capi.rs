//! The C ABI: sequences of calls with valid, invalid and NULL arguments
//! and mismatched struct sizes.
#![no_main]

#[path = "../../tests/harness/mod.rs"]
mod harness;

libfuzzer_sys::fuzz_target!(|data: &[u8]| harness::capi(data));
