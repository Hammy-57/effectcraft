title: test: one integration-test binary per crate and UI panel fingerprints
branch: upstream/agents-ui-test-binary
depends: upstream/agents-control-robustness

## What
- Crates with many integration-test files build them as one binary: `ui-egui` (`--test ui`), `host`,
  `export` and `effects`. `tests/<name>/main.rs` declares each `tests/*.rs` with `#[path]`;
  `every_test_file_is_a_module` fails when a file is left out. Tests that create a GPU device hold
  `gpu_lock()` (wgpu's GL backend crashes with concurrent devices). `profile.dev` uses
  line-tables-only debug info for workspace crates.
- UI panel fingerprint tests: a compact fingerprint of every panel's widgets, pinned in
  `ui_fingerprints.txt`, so an unintended UI change shows up as a test diff.

## Why
The ui-egui tests linked the whole app ~50 times; one binary builds once. The fingerprints catch
accidental UI regressions without screenshots.

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

Note: `ui_fingerprints.txt` pins the panels as rendered on aarch64 macOS (elsewhere the test only
checks that no panel renders flat). The pins were recorded on the fork's UI, so they need re-pinning
on a Mac against upstream's UI (`UI_FINGERPRINT_PRINT=1 cargo test -p effectcraft-ui-egui --test ui
ui_fingerprints::`) before this lands.
