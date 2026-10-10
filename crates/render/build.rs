//! Self-registering modules (see `effectcraft-buildgen`): every `src/*.rs` file is a public
//! module of the crate; `tests*.rs` files are `#[cfg(test)]` modules.

use effectcraft_buildgen::Modules;

fn main() -> Result<(), String> {
    Modules::new("src", "src/lib.rs", "modules.rs").visibility("pub").generate()
}
