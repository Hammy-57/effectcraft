title: feat(mcp): headless MCP tools: verification, layout, offscreen UI, render views and a robust control channel
branch: upstream/agents-control-robustness
depends: none

The first of three PRs that make EffectCraft easier for AI agents to drive and check. This one
covers the MCP server's tools and the control channel; the next two build on it.

## What
**CI for contributors and agents**
- `cargo xtask ci --affected [--base REV] [--dry-run]`: the CI gates (fmt, clippy, tests via nextest
  when installed, doctests, layers, assets, wasm) for only the packages a change can affect: changed
  files map to their package and to packages that read them through `../` paths, expanded to reverse
  dependents. Workspace-wide inputs (root `Cargo.toml`, `Cargo.lock`, `.cargo/`, `xtask/`, toolchain,
  `clippy.toml`) fall back to the full CI.
- `[profile.ci]` (release without LTO, 16 codegen units): `xtask ci` builds clippy and tests with it
  instead of `--release`. Three wall-clock asserts (engine memory tick, GPU OpenGL decline, script
  socket) keep their intent with bounds that survive a loaded machine.

**Verification**
- MCP structured results: for clients that negotiate 2025-06-18 or later, JSON tool results also
  come as `structuredContent`, and the tools with a stable shape declare an `outputSchema`.
- Repro scripts: `journal.export` writes the session's command journal as a JSON-lines replay script;
  `effectcraft-cli replay SCRIPT.jsonl [--keep-going]` replays one headless. `tests/repros/*.jsonl`
  are replayed by a host test, so a crash fix can ship its repro as a file.
- `render.snapshot` / `render.compare` (MCP `render_snapshot` / `render_compare`, CLI
  `effectcraft-cli compare A B`): keep a rendered frame and compare frames, image files or renders of
  now: max / mean difference, PSNR, SSIM, changed pixels, pass/fail, a diff heatmap; `region` and
  `diffBounds`.

**Layout and long calls**
- `layer.bounds` / MCP `get_layout`: where layers are on screen at a time without rendering (visible
  and why not, comp-space bounds, corners, centre, on-screen fraction, optional drawn-pixel bounds and
  overlaps), after parenting and the active camera.
- Progress and cancellation: renders, analyses (tracking, warp stabilizer, camera tracker, roto
  propagate, content fill, scene detection, footage checks), `jobs.wait` and scripts run as
  background jobs while the server keeps reading; `notifications/progress`, `notifications/cancelled`,
  `ping` served meanwhile. A second render while one runs is still refused at once.

**Offscreen UI, resources and prompts**
- `effectcraft-cli mcp --ui`: the full app runs offscreen (egui_kittest + wgpu, no window), so
  `screenshot` and the `ui_*` tools work headless; automation gains a `ControlTransport` trait and
  stays free of egui (the ui-egui `offscreen` feature carries the offscreen app). The `snapshot`
  example reuses it; `mcp --ui --autosave`.
- Resource templates (`comp/{comp}`, `layer/{comp}/{layer}`, `frame/{comp}/{time}` as a PNG blob,
  `command/{id}`), prompts (`title_card`, `lower_third`, `port_ae_script`, `check_against_reference`,
  `animate_layer`), resource subscriptions and logging notifications.
- `audio.analyze` / MCP `analyze_audio`: peak, true peak, RMS, integrated LUFS, clipping, silences
  and an optional waveform image.
- Duplicate tool aliases stay callable but leave `tools/list` (`mcp --all-tools` lists them);
  `render_preview` takes `path` and `inline`.

**Render views**
- `render.frame` and the MCP render tools take `region`, `layer` (one layer solo in place), `item`
  (a footage or solid item on its own) and `overlays` (bounds, anchors, motion paths, safe margins,
  grid, with each layer's comp-space bounds in the reply).
- `render.contactSheet` / MCP `render_frames`: several labelled frames on one PNG (optionally an
  animated GIF).

**Control channel robustness**
- Stable layer ids for agents (`"#n"` is the stack index, a bare number is a layer id), per-client
  undo bookkeeping, a document `revision`, and Layer Style Cancel that keeps other clients' edits.
- A request that times out before the UI thread starts it is cancelled and never runs; a top-level
  `requestId` answers retries from a reply cache (MCP `_meta.requestId`), so a retried call never
  runs twice; `events.subscribe` streams `project.changed` / `selection.changed`; a busy control port
  is reported; `ui.screenshot` takes `max_side`; `app.revision`; `--bridge` / `--control` without a
  port use `EFFECTCRAFT_CONTROL_PORT` (else 9877).
- Agent-friendly errors: clamped values come back as `warnings`, errors name the argument and suggest
  a fix, index and enum parameters have typed schemas, catalogues have a cheap `brief` mode, and
  mutations return what they changed.

## Why
Agents need machine-readable results and a way to prove what an edit changed (snapshot → edit →
compare); an agent checking a layout should not have to render and eyeball a frame; a long render
should not freeze the MCP session; agents could only see the UI with a desktop app running; and
agents retry on timeouts and act on stale indices, so retries must be safe, references stable and
mistakes self-explaining. Bug reports get a replayable script instead of prose, and per-change CI
no longer takes as long as a release build.

## How tested
- `rustfmt --check` and `cargo check` of the affected crates on this branch.
- On the top of the three-PR series (this branch plus the next two): `cargo clippy --all-targets
  -- -D warnings` for automation, engine, cli, ui-egui, effectcraft, host, export, effects, project,
  raster, script, gpu and xtask: clean. `cargo test`: automation 77/77, cli (unit 3/3, cli 18/18,
  mcp tests), engine lib 596 passed (2 ignored), raster, host (including `tests/repros`) and xtask
  pass.
- Not run: the `ui-egui` integration tests (`--test ui`) and the full `cargo xtask ci` (the runner
  had no GPU and 4 cores).

Cherry-picked with `-x` trailers and adapted to current `main`: new modules are declared by hand in
`lib.rs` / `commands/mod.rs`.
