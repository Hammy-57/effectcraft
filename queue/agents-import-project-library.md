title: feat(engine): import a project into a project; library.* commands
branch: upstream/agents-import-project-library
depends: upstream/agents-ui-test-binary

## What
- `file.importProject` (File ▸ Import ▸ EffectCraft Project..., and `.ecproj` / `.ecprojx` given to
  File ▸ Import): merges another project into a folder named after the file, with every id shifted
  past the open project's, in one undo step; relative footage resolves next to the imported file.
  Id offsets saturate instead of overflowing on damaged files.
- `library.*` engine commands (generic, off until a folder is added): folders indexed by a
  `library.json` hold reusable comps, presets, footage and sounds; `library.search` ranks entries,
  `library.insert` imports and nests a comp, adds media as a layer or applies a preset;
  `library.addFolder` / `removeFolder` / `folders` / `get`. Docs: `docs/libraries.md`.

## Why
Reusing work across projects (After Effects imports `.aep` into a project the same way), and a plain
local-folder library that people and agents can search and insert from. No library ships with this
change.

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
