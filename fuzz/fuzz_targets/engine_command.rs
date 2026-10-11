#![no_main]

libfuzzer_sys::fuzz_target!(|data: &[u8]| effectcraft_fuzz::engine_command(data));
