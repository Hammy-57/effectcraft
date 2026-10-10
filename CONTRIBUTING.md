# Contributing to EffectCraft

Thanks for helping. Please read [AGENTS.md](AGENTS.md) first: its rules on assets and clean-room
work apply to everyone, people and AI agents alike, and they are not negotiable.

## Setup

You need [Rust](https://rustup.rs/) 1.95 or newer.

```sh
cargo run -p effectcraft -- --demo       # the desktop app, with the demo project
cargo test --workspace
cargo xtask ci                           # what every commit must pass
```

## Ground rules

- **Clean room.** Work from After Effects' public documentation and its observable behaviour only.
  Never use Adobe artwork, presets or code, and never copy GPL/LGPL/AGPL code.
- **Every asset is attributed.** Add a `.attribution` sidecar and an [ATTRIBUTION.md](ATTRIBUTION.md)
  row; `cargo xtask assets` checks it.
- **Layering.** Nothing below the interface layer depends on UI or OS crates; see
  [docs/architecture.md](docs/architecture.md).
- **Everything is a command.** New features are engine commands with an id, label, menu path,
  shortcut and parameters, so the UI, the CLI and the MCP server get them at once. Interactive
  widgets register an automation id.
- **Never crash.** Non-test code returns errors instead of panicking: no `unwrap()`, `expect()`,
  `panic!`, `unreachable!`, `todo!`, `unimplemented!` or `unsafe`, checked indexing and arithmetic on
  input-derived values, and a regression test with every crash fix. See the "Never crash" section of
  [AGENTS.md](AGENTS.md).
- **Tests with every change**, and green `cargo xtask ci` before every commit. One task per commit.

## How to add…

- **An effect:** add an `EffectSpec` in `crates/effects` (AE display name, category, parameters
  with AE's names, defaults and ranges) and its render function, with a determinism test and a
  behaviour test. If it reads the clock directly, add it to `TIME_DEPENDENT`. A new file
  `crates/effects/src/<name>.rs` with `pub fn specs() -> Vec<EffectSpec>` registers itself.
- **A command:** register it with `cmd!` in `crates/engine/src/commands/`, place it in the menu
  tree in `crates/engine/src/menus.rs`, and test it including undo. A new file
  `crates/engine/src/commands/<name>.rs` with `pub fn specs() -> Vec<CommandSpec>` registers itself.
- **A panel or dialog:** `crates/ui-egui/src/panels/`; look at the result with the headless
  snapshot tool described in [docs/testing.md](docs/testing.md).
- **A test file:** `src/tests_<topic>.rs` next to `lib.rs` (engine, render) is picked up on its own.

New module files need no `mod` line ([self-registering modules](docs/architecture.md#self-registering-modules));
format with `cargo xtask fmt`, since `cargo fmt` doesn't reach them. Parity history is appended
to [docs/parity-log.md](docs/parity-log.md) (union-merged); [docs/parity.md](docs/parity.md) keeps
the current state only.

## Talk to us

Questions, ideas and bug reports are welcome on [Discord](https://discord.gg/artcraft) and in
GitHub issues.
