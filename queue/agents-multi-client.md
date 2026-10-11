title: feat(automation): per-agent cursors, comp claims and agent names
branch: upstream/agents-multi-client
depends: upstream/agents-lint-motion

## What
- Per-agent cursors: commands carrying a `client` work on that client's own selection, active comp,
  current time and text editing, swapped in for the command; the user's view never moves.
- Comp claims: `comp.claim` / `comp.release` / `comp.claims` reserve a comp against other clients'
  edits (expiring; released when the client's last control connection closes).
- Every MCP tool takes `client` (and `mcp --client NAME` / `EFFECTCRAFT_CLIENT`); bridged MCP servers
  get their own agent name by default, so an agent never moves the user's selection.
- MCP recovery saves stay bounded on disk.

## Why
Several agents, or an agent and a person, working in one project stepped on each other's selection,
time and edits.

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
