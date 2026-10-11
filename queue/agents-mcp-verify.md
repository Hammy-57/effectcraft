title: feat(mcp): structured results, repro replay scripts, render snapshot and compare
branch: upstream/agents-mcp-verify
depends: upstream/agents-ci-affected

## What
- MCP structured results: for clients that negotiate 2025-06-18 or later, JSON tool results also come
  as `structuredContent`, and the tools with a stable shape declare an `outputSchema`.
- Repro scripts: `journal.export` writes the session's command journal as a JSON-lines replay script;
  `effectcraft-cli replay SCRIPT.jsonl [--keep-going]` replays one headless (per-step result, first
  failure, exit 1 on a failed expectation or a panic). `tests/repros/*.jsonl` are replayed by a host
  test, so a crash fix can ship its repro as a file.
- `render.snapshot` / `render.compare` (MCP `render_snapshot` / `render_compare`, CLI
  `effectcraft-cli compare A B`): keep a rendered frame and compare frames, image files or renders
  of now: max / mean difference, PSNR, SSIM, changed pixels, pass/fail, and a diff heatmap.

## Why
Agents need machine-readable results and a way to prove what an edit changed (snapshot → edit →
compare); bug reports need a replayable script instead of prose.

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
