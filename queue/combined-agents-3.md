title: feat(automation): several agents in one project: own cursors, comp claims, shared hosts and own-step undo
branch: upstream/agents-own-undo
depends: upstream/agents-lint-motion

The third of three PRs for AI agents: the multi-agent model. It builds on the previous two; once
they are merged, this diff is only the parts below.

## What
**Per-agent cursors and claims**
- Commands carrying a `client` work on that client's own selection, active comp, current time and
  text editing, swapped in for the command; the user's view never moves.
- `comp.claim` / `comp.release` / `comp.claims` reserve a comp against other clients' edits
  (expiring; released when the client's last control connection closes).
- Every MCP tool takes `client` (and `mcp --client NAME` / `EFFECTCRAFT_CLIENT`); bridged MCP
  servers get their own agent name by default. MCP recovery saves stay bounded on disk.

**Hosts and background jobs**
- Control-channel job commands (`jobs.wait`, renders, imports) reply when the job ends instead of
  blocking the UI thread; renders requested over the channel run off the UI thread; a bad background
  render request fails up front. Big imports (`file.importProject`) run in the background for agents.
- A host list (`<config>/Hosts`): each running EffectCraft announces its port and project; saving a
  project that another host has open is guarded. MCP `hosts {join?}` lists them and joins one.
- `effectcraft-cli serve [F.ecproj | --demo] [--control PORT]`: a windowless host several agents
  share over the control channel, with checkpoints; the desktop app takes the project over when it
  opens it.

**Undo your own step, turn-taking**
- `edit.undo` with a client (and the user's ⌘Z) reverts the caller's latest step even when others
  worked after it, as a new step, layer by layer inside a comp, and only where nobody changed the
  same thing since; otherwise it refuses and names who did. ⌘Z again goes on to the step before. The
  Undo menu names your own step.
- Requests from different clients are served fairly; the app's control server and the windowless
  host share one control-channel wire and job waiter.
- Confined MCP sessions (`--automation-read-root` / `--automation-write-root`) also check the file
  writes of the render, audio, journal and repro commands and the project / library reads.

## Why
Several agents, or an agent and a person, working in one project stepped on each other's selection,
time and edits, kept separate copies instead of one live project, and plain undo removed whoever made
the last step.

## How tested
- `rustfmt --check` and `cargo check` of the affected crates on this branch.
- On this branch (the top of the series): `cargo clippy --all-targets -- -D warnings` for
  automation, engine, cli, ui-egui, effectcraft, host, export, effects, project, raster, script, gpu
  and xtask: clean. `cargo test`: automation 77/77, cli (unit 3/3, cli 18/18, mcp tests), engine lib
  596 passed (2 ignored), raster, host and xtask pass.
- Not run: the `ui-egui` integration tests (`--test ui`) and the full `cargo xtask ci` (the runner
  had no GPU and 4 cores).

Cherry-picked with `-x` trailers and adapted to current `main`.
