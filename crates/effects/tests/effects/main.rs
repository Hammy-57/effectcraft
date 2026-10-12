//! One test binary for the effectcraft-effects integration tests: every `tests/*.rs` file is a
//! module here, so the effects is linked once instead of once per file (`autotests = false`).
//!
//! - Run one file's tests: `cargo test -p effectcraft-effects --test effects sim_golden::` (a name filter).
//! - A new `tests/x.rs` needs its two lines below (`#[path = "../x.rs"] mod x;`); cargo ignores it
//!   otherwise, so `every_test_file_is_a_module` fails until it has them.
//! - Tests of different files share one process and run in parallel.

#[test]
fn every_test_file_is_a_module() {
    let main = include_str!("main.rs");
    let missing: Vec<String> = std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/tests"))
        .unwrap()
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|f| f.ends_with(".rs") && !main.contains(&format!("#[path = \"../{f}\"]")))
        .collect();
    assert!(missing.is_empty(), "not compiled: declare each in tests/effects/main.rs (`#[path = \"../x.rs\"] mod x;`): {missing:?}");
}

#[path = "../gen_golden.rs"]
mod gen_golden;
#[path = "../particle_golden.rs"]
mod particle_golden;
#[path = "../sim_golden.rs"]
mod sim_golden;
#[path = "../timewarp_golden.rs"]
mod timewarp_golden;
