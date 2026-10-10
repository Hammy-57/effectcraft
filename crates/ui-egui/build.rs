//! Self-registering modules (see `effectcraft-buildgen`): every `src/*.rs` and
//! `src/panels/*.rs` file is a public module. Adding a panel module is adding a file.

use effectcraft_buildgen::Modules;

fn main() -> Result<(), String> {
    Modules::new("src", "src/lib.rs", "modules.rs").visibility("pub").generate()?;
    Modules::new("src/panels", "src/panels/mod.rs", "panels.rs").visibility("pub").generate()
}
