title: feat(mcp): layer geometry (get_layout) and progress/cancellation for long calls
branch: upstream/agents-mcp-layout-progress
depends: upstream/agents-mcp-verify

## What
- `layer.bounds` / MCP `get_layout`: where layers are on screen at a time without rendering
  (visible and why not, comp-space bounds, corners, centre, on-screen fraction, optional drawn-pixel
  bounds and overlaps), after parenting and the active camera.
- Progress and cancellation for long MCP calls: renders, analyses (tracking, warp stabilizer,
  camera tracker, roto propagate, content fill, scene detection, footage checks), `jobs.wait` and
  scripts run as background jobs while the server keeps reading; `notifications/progress` with a
  `progressToken`, `notifications/cancelled` stops them, `ping` and other requests are served
  meanwhile. Upstream's rule stays: a second render while one runs is refused at once.

## Why
An agent checking a layout should not have to render and eyeball a frame, and a long render or
analysis should not freeze the MCP session or be impossible to stop.

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
