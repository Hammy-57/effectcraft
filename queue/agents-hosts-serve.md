title: feat(automation): host list, windowless serve host, handover, background jobs
branch: upstream/agents-hosts-serve
depends: upstream/agents-multi-client

## What
- Control-channel job commands (`jobs.wait`, renders, imports) reply when the job ends instead of
  blocking the UI thread; renders requested over the channel run off the UI thread; a bad background
  render request fails up front.
- A host list (`<config>/Hosts`): each running EffectCraft announces its port and project; saving a
  project that another host has open is guarded.
- MCP `hosts {join?}`: list where projects are open and join one as your own client.
- `effectcraft-cli serve [F.ecproj | --demo] [--control PORT]`: a windowless host several agents
  share over the control channel, with checkpoints; the desktop app takes the project over when it
  opens it.
- Big imports (`file.importProject`) run in the background for agents.

## Why
Agents and people need one shared, live project, with or without a window, instead of separate
copies.

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
