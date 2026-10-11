title: feat(xtask): ci --affected and a faster CI test profile
branch: upstream/agents-ci-affected
depends: none

## What
- `cargo xtask ci --affected [--base REV] [--dry-run]`: the CI gates (fmt, clippy, tests via
  nextest when installed, doctests, layers, assets, wasm) for only the packages a change can affect:
  changed files map to their package and to packages that read them through `../` paths, expanded to
  reverse dependents. Workspace-wide inputs (root `Cargo.toml`, `Cargo.lock`, `.cargo/`, `xtask/`,
  toolchain, `clippy.toml`) fall back to the full CI.
- `[profile.ci]` (release without LTO, 16 codegen units): `xtask ci` builds clippy and tests with it
  instead of `--release`.
- Three wall-clock asserts (engine memory tick, GPU OpenGL decline, script socket) keep their intent
  with bounds that survive a loaded machine.

## Why
Full CI rebuilds every test binary with thin LTO and one codegen unit; per-change checks of a few
crates were taking as long as a release build. Contributors and agents can gate a change on what it
touches and keep the full run for releases.

## How tested
- `rustfmt --check` and `cargo check` of the affected crates on this branch (queue-pr.sh).
- On the top of the stacked series this PR starts or belongs to
  (`upstream/agents-own-undo`): `cargo clippy --all-targets -- -D warnings` for automation, engine,
  cli, ui-egui, effectcraft, host, export, effects, project, raster, script, gpu and xtask; and
  `cargo test` for automation, cli, engine, raster, host and xtask. Results: clippy clean; automation 77/77, cli (unit 3/3, cli 18/18, mcp tests), engine lib 596 passed (2 ignored), raster, host (including tests/repros) and xtask all pass.
- Not run here: the `ui-egui` integration tests (`--test ui`) and the full `cargo xtask ci`
  (the runner used had no GPU and 4 cores).

Cherry-picked from eliasx45/effectcraft (`-x` trailers) and adapted to current upstream `main`:
new modules are declared by hand in `lib.rs` / `commands/mod.rs` (the fork's auto-registration
build step is not part of this series).
