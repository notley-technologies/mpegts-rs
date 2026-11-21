//! The write primitives: random tables, streams, units and sinks; what is
//! written must obey the packet rules and read back unchanged.
#![no_main]

#[path = "../../tests/harness/mod.rs"]
mod harness;

libfuzzer_sys::fuzz_target!(|data: &[u8]| harness::mux(data));
