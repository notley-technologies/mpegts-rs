//! Video elementary stream bytes: NAL units, SEI messages and user data of
//! H.264, H.265 and MPEG-2, emulation prevention and SEI rebuilding.
#![no_main]

#[path = "../../tests/harness/mod.rs"]
mod harness;

libfuzzer_sys::fuzz_target!(|data: &[u8]| harness::nal(data));
