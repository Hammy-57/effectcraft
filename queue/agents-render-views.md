title: feat(mcp): render regions, solo layers, items, overlays and contact sheets
branch: upstream/agents-render-views
depends: upstream/agents-mcp-offscreen-ui

## What
- `render.frame` (and the MCP render tools) take `region` (a rectangle at full resolution), `layer`
  (one layer solo in place), `item` (a footage or solid item on its own) and `overlays` (layer bounds,
  anchors, motion paths, safe margins, grid, with each layer's comp-space bounds in the reply).
- `render.contactSheet` / MCP `render_frames`: several labelled frames on one PNG (and optionally an
  animated GIF).
- `render.compare` takes a `region` and returns `diffBounds`; output schemas for the render tools.

## Why
Checking a detail of a 4K comp, one layer in isolation, or an animation's timing took several full
renders; one call now shows it.

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
