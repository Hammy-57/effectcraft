title: feat(mcp): design lint (lint_frame) and motion summary (get_motion)
branch: upstream/agents-lint-motion
depends: upstream/agents-exam

## What
- Design lint: `render.lint` / MCP `lint_frame` reports plain-language design problems at a time or
  over a range: text too small or low contrast (WCAG ratio against a text-free render), outside title
  or action safe, cut off, overlapping text, layers switched on but hidden, keyframes just off the
  music's beat (a small onset/tempo tracker).
- Motion summary: `comp.motion` / `layer.motion` / MCP `get_motion` describe how layers move over a
  range without rendering: per-layer segments (enter, exit, move, hold, scale, rotate, fades),
  off-screen intervals, a one-sentence summary, and overlaps.
- Two exam tasks use them end to end.

## Why
Agents cannot judge a frame by eye; these give them checkable facts about legibility, layout and
choreography.

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
