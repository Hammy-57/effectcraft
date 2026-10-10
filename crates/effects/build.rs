//! Self-registering modules (see `effectcraft-buildgen`): every `src/*.rs` file is a module of
//! the crate, and every one with a top-level `fn specs()` is in the effect registry. Adding
//! effects is adding a file.

use effectcraft_buildgen::Modules;

fn main() -> Result<(), String> {
    Modules::new("src", "src/lib.rs", "modules.rs").registry("EFFECT_MODULES", "specs", "fn() -> Vec<crate::EffectSpec>").generate()
}
