title: fix: crash and hang fixes from fuzzing, Puppet overlay off the UI thread, memory release on open, CLI and test fixes
branch: upstream/combined-fixes
depends: none

## What
**Never crash: fuzzing**
- `fuzz/`: cargo-fuzz targets for every input EffectCraft doesn't control: `project_open` (`.ecproj`
  JSON and XML, loaded and rendered), `lottie_import`, `svg_parse`, `psd_parse`, `pdf_parse` (PDF /
  AI / EPS), `interchange_import` (FCP7 XML, FCPXML, OTIO, EDL), `expr_eval`, `media_probe` and
  `engine_command` (commands with fuzzed parameters on the demo project). File services deny every
  access; nothing touches the disk, network or processes. The crate has its own `[workspace]`, so
  normal builds and CI never compile it.
- `cargo xtask fuzz --list | --seeds | [TARGET…] [--seconds N] [--jobs N]`, a seed corpus
  (`fuzz/corpus/*/seed-*`, attributed in ATTRIBUTION.md), docs/testing.md "Fuzzing", findings under
  G3 in docs/gaps.md. Most of these lines are `fuzz/Cargo.lock` and seed files.
- PDF: a page tree whose `Pages` node lists itself in `Kids` was walked 3^32 times and hung the
  import; the walk now has a visit budget.
- Paths: shapes with huge coordinates (e.g. 1e30) are clipped to a safe range before tiny-skia
  rasterises them, instead of overflowing or hanging the rasteriser.

**Puppet responsiveness (#586)**
- The Puppet mesh solve is reused when only unrelated inputs change; a mesh cache hit no longer
  allocates the alpha buffer.
- A changing Puppet overlay (dragging a pin, scrubbing) is solved off the UI thread through a bounded
  mailbox on the existing frame pool; intermediate poses coalesce, and a failed or panicking solve
  shows a status message instead of taking the app down.
- `Session.epoch` counts project replacements, so the overlay cache can't show a stale mesh after
  opening another project whose comp/layer ids match.

**Memory (#591)**
- `Session::replace_project` (open, new) clears the layer cache and sends `Event::PurgeCaches`; the
  app's handler also clears the GPU upload cache. Edit ▸ Purge ▸ Memory clears it too.

**Settings and CLI**
- Settings ▸ Scripting & Expressions ▸ Agents ▸ **Let Agents Control EffectCraft**
  (`scripting.agentControl`, default off) and **Control Channel Port** (`scripting.agentControlPort`,
  default 9877): the app starts the localhost control channel on every launch; `--control` still wins.
- `effectcraft-cli render --comp <x>` resolves the comp like `render-frame`: a number is a comp id,
  `-` the active comp, anything else a name.

**Tests**
- The 1080p tracking test keeps its accuracy check in the normal suite; its 100 ms/frame budget is a
  separate `#[ignore]` performance test, documented in docs/testing.md.
- Regression test for #637: double-clicking a precomp layer in the viewer opens it in its own
  Timeline tab and makes it active.

## Why
Imported files are input we don't control, and they had no fuzzing; the first minute per target
found the PDF and path hangs fixed here. Dragging a Puppet pin solved the mesh on the UI thread
every frame (#586). Opening a project kept the previous document's layer buffers and GPU uploads
alive (#591). `render --comp 2` was read as the name "2". Agents needed the app launched with
`--control` every time. The tracking budget failed under a parallel suite without any regression.

## How tested
- `rustfmt --edition 2024 --check` on every changed `.rs` file (fuzz crate included): clean.
- `cargo clippy --all-targets -- -D warnings` for effects, engine, path, pdf, track, ui-egui,
  effectcraft, effectcraft-cli and xtask: clean.
- `cargo check --target wasm32-unknown-unknown` for effects, engine, path, pdf and track: clean.
- `cargo test` for the same nine packages: 1,709 passed. One Puppet viewer test
  (`puppet_bend_pin_rotates_by_its_ring_and_scales_by_its_square`) read handle geometry from the
  previous paint pass while the async solve moved it; the last commit makes headless lookups use the
  completed paint pass and adds a regression test, after which ui-egui, effectcraft and
  effectcraft-cli pass again (552 passed).
- Known failure, also on `main`: `theme::script_font_tests::arabic_words_are_joined_and_read_right_to_left`.
- Not run: the full `cargo xtask ci`. Tests ran on a 4-core Linux runner without a GPU (wgpu on
  llvmpipe).

Cherry-picked with `-x` trailers from topic branches already adapted to current `main`.
