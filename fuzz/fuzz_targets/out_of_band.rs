#![no_main]
libfuzzer_sys::fuzz_target!(|data: &[u8]| stvtf::fuzzing::out_of_band(data));
