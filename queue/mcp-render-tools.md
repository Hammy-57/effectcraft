title: feat(mcp): render_snapshot/render_compare, contact sheets and region, solo and overlay renders
branch: upstream/mcp-render-tools
depends: upstream/mcp-protocol

## What
Agents can verify visual work by looking, and render more than the whole comp:
- `render.snapshot` / `render.compare` (MCP `render_snapshot` / `render_compare`, CLI `effectcraft-cli compare a.png b.png`): keep named frames in the session and compare snapshots, PNG/JPEG files or frames rendered now. The reply has max/mean difference, PSNR, SSIM, changed pixels, `pass`, `region`/`diffBounds` and a diff heatmap. The metrics live in `effectcraft-raster::compare` (pure Rust, wasm-safe).
- `render.frame` becomes an engine command with `region`, `layer` (solo), `item` (a footage or solid item on its own) and `overlays` (layer bounds, anchors, motion paths, safe margins, grid), so the MCP bridge and headless sessions render the same thing.
- `render.contactSheet` / `render_frames`: several frames on one labelled sheet, optionally an animated GIF.
- Output schemas for `render_snapshot` and `render_compare`.

## Why
After an edit an agent needs a cheap, objective answer to "did this change only what I meant" and "what does this region look like at full resolution", without pulling a full-size frame into its context each time.

## How tested
- `cargo test -p effectcraft-automation --lib` (56 passed)
- `cargo test -p effectcraft-engine --lib -- render_view render_compare` (15 passed)
- `cargo test -p effectcraft-raster --lib -- compare` (8 passed)
- `cargo check` of automation, cli, engine, raster and ui-egui with `--tests`.
Not run: the CLI integration tests (`effectcraft-cli compare`, `render --out`) and the rest of the suites. No `cargo xtask ci`.

Depends on the MCP protocol PR (`upstream/mcp-protocol`: output schemas, `render_preview`); open this one after it is merged.

Split out of #859 at the maintainer's request on #858.
