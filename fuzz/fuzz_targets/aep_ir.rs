#![no_main]

libfuzzer_sys::fuzz_target!(|data: &[u8]| effectcraft_fuzz::aep_ir(data));
