title: feat(automation): own-step undo and turn-taking for several agents
branch: upstream/agents-own-undo
depends: upstream/agents-hosts-serve

## What
- Undo your own step: `edit.undo` with a client (and the user's ⌘Z) reverts the caller's latest step
  even when others worked after it, as a new step, layer by layer inside a comp, and only where nobody
  changed the same thing since; otherwise it refuses and names who did. ⌘Z again goes on to the step
  before. The Undo menu names your own step.
- Agents take turns on the control channel (requests from different clients are served fairly);
  the app's control server and the windowless host share one control-channel wire and job waiter.
- Confined MCP sessions (`--automation-read-root/--automation-write-root`) also check the file
  writes of the render, audio, journal and repro commands and the project/library reads.

## Why
With agents and a person in one project, plain undo removed whoever made the last step.

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
