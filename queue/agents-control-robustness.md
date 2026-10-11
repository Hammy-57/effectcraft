title: feat(automation): stable layer ids, control-channel retries and events, agent-friendly errors
branch: upstream/agents-control-robustness
depends: upstream/agents-render-views

## What
- Stable layer ids for agents (`"#n"` is the stack index, a bare number is a layer id), per-client
  undo bookkeeping, a document `revision`, and Layer Style dialog Cancel that keeps other clients'
  edits.
- Control channel: a request that times out before the UI thread starts it is cancelled and never
  runs; a top-level `requestId` answers retries from a reply cache (MCP: `_meta.requestId` /
  `request_id`, so a retried tool call never runs twice); `events.subscribe` streams
  `project.changed` / `selection.changed`; a busy control port is reported; `ui.screenshot` takes
  `max_side`; `app.revision`; `--bridge` / `--control` without a port use `EFFECTCRAFT_CONTROL_PORT`
  (else 9877).
- Agent-friendly errors: clamped values come back as `warnings`, errors name the argument and suggest
  a fix, index and enum parameters have typed schemas, the catalogues have a cheap `brief` mode, and
  mutations return what they changed.
- The `snapshot` example reuses the offscreen app; `mcp --ui --autosave`.

## Why
Agents retry on timeouts and act on stale indices; these make retries safe, references stable and
mistakes self-explaining.

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
