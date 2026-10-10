//! Self-registering modules (see `effectcraft-buildgen`): every `src/*.rs` file is a module of
//! the crate (`tests*.rs` / `*_tests.rs` are `#[cfg(test)]`), and every `src/commands/*.rs` file
//! is a command module whose `specs()` is registered. Adding a command is adding a file.

use effectcraft_buildgen::Modules;

fn main() -> Result<(), String> {
    Modules::new("src", "src/lib.rs", "modules.rs").visibility("pub").registry("COMMAND_MODULES", "specs", "fn() -> Vec<crate::CommandSpec>").generate()?;
    Modules::new("src/commands", "src/commands/mod.rs", "commands.rs").registry("COMMAND_MODULES", "specs", "fn() -> Vec<crate::CommandSpec>").generate()
}
