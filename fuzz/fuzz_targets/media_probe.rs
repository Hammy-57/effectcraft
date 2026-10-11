#![no_main]

libfuzzer_sys::fuzz_target!(|data: &[u8]| effectcraft_fuzz::media_probe(data));
