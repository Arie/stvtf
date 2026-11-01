#![no_main]
libfuzzer_sys::fuzz_target!(|data: &[u8]| stvtf::fuzzing::channel(data));
