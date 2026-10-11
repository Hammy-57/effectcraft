title: feat(mcp): offscreen UI, resource templates, prompts, analyze_audio and notifications
branch: upstream/agents-mcp-offscreen-ui
depends: upstream/agents-mcp-layout-progress

## What
- `effectcraft-cli mcp --ui`: the full app runs offscreen (egui_kittest + wgpu, no window), so
  `screenshot` and the `ui_*` tools work headless; automation gains a `ControlTransport` trait (the
  TCP bridge is one, the offscreen app another) and stays free of egui. The ui-egui `offscreen`
  feature carries the offscreen app.
- MCP resource templates (`comp/{comp}`, `layer/{comp}/{layer}`, `frame/{comp}/{time}` as a PNG blob,
  `command/{id}`) and prompts (`title_card`, `lower_third`, `port_ae_script`,
  `check_against_reference`, `animate_layer`).
- `audio.analyze` / MCP `analyze_audio`: peak, true peak, RMS, integrated LUFS, clipping, silences and
  an optional waveform image.
- MCP notifications: resource subscriptions (`notifications/resources/updated` on any subscribed
  project URI when the revision changes) and logging (`logging/setLevel`, expression errors, missing
  footage, auto-save failures).
- Duplicate tool aliases (`list_commands`, `execute_command`, `get_project`, `render_frame`) stay
  callable but leave `tools/list` (`mcp --all-tools` lists them); `render_preview` takes `path` and
  `inline`.

## Why
Agents could only see and operate the UI with a desktop app running; MCP clients could not discover
resources and prompts or follow changes.

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
