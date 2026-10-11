title: feat: Figma import, project import, Effect Console, relink moved footage, shape and timeline editing, AZERTY shortcuts, Graph Editor zoom and a render benchmark
branch: upstream/combined-features
depends: none

## What
**Effects**
- Effect Console (Edit ▸ Effect Console…, Ctrl+Space; control channel `app.fxConsole {query?}`): a
  search bar at the pointer ranking effects and presets by name, word starts, initials (`gb` =
  Gaussian Blur) and letters in order, with recent and favourite effects first. Enter applies and
  closes, Shift+Enter applies and stays open.
- CC Particle World / CC Particle Systems II replace their layer with the particles (the Transfer
  Mode only combines particles with each other), as in After Effects; particle golden hashes
  re-pinned.
- Time effects (Echo, Posterize Time, ...) see the effects applied before them in the stack.

**Import**
- `file.importFigma`: Figma frames and layers become native, editable layers (comps and precomps,
  group nulls, shape layers with editable paths, fills, gradients and strokes, text with style runs,
  track mattes, layer styles, Gaussian Blur; image fills and unsupported nodes as 2× PNG renders),
  from a Figma link through the REST API (system `curl`; the token comes from `FIGMA_TOKEN` /
  `figma.setToken` and is never journaled) or node JSON; one undo step. `effectcraft-cli
  figma-bridge` is a local HTTP endpoint for the Figma plug-in in `integrations/figma/`.
- File ▸ Import ▸ EffectCraft Project… (`file.importProject {path}`; `.ecproj` / `.ecprojx` given to
  File ▸ Import): merges a project into the open one, in a folder named after the file, every id moved
  past the open project's, one undo step, relative footage resolved next to the imported file;
  `offset_ids` also shifts tracker Motion Targets and saturates instead of overflowing.

**Footage**
- `footage.relink {item, path}` and File ▸ Replace Footage on a missing item relink it and every
  other missing item in the same relative place (moved folder, renamed drive), in one undo step;
  `footage.missing`; the footage check after File ▸ Open finds files that moved with the project
  folder; Windows and macOS paths compare by their parts.
- The SVG rasteriser paints each fill and stroke only inside its own bounding box instead of a
  whole-canvas mask (a 4001×2501 Illustrator layer: about 556 s → 5–7 s per frame).

**Shapes**
- Toolbar Fill / Stroke apply to selected shape layers: `shape.fillStroke` takes `fillType` /
  `strokeType` (`none|solid|linear|radial`), blend modes and opacities, and applies them to every
  Fill / Stroke and Gradient Fill / Stroke, nested groups included, in one undo step (None switches
  items off; Linear / Radial convert in place).
- Reverse Path Direction (⇄) on Path, Rectangle, Ellipse and Polystar rows:
  `shape.reversePathDirection {layer?, prop?, reversed?}`, one undo step; scripts set `ADBE Vector
  Shape Direction` with After Effects' values (1 Default, 3 Reversed).

- Properties panel: shape layers get a Layer Contents section (paths, Fill, Stroke); solids get a
  Roundness row that turns the solid into a rounded-rectangle shape layer in one undo step;
  `layer.roundCorners {layer?, roundness}` (Layer ▸ Create ▸ Round Corners, translated in the four
  catalogues); long row labels push their values right instead of overlapping.

**Timeline and shortcuts**
- Shortcut presets accept `Physical+<key>` (US-position names), so AZERTY's `-` and `)` bind
  separately; a focused panel's binding wins over an app command on the same chord; `ui.key` accepts
  the notation. New `time.workAreaStart` / `time.workAreaEnd` (Shift+Home / Shift+End).
- Layer bars and the Graph Editor share one time zoom; the footer's zoom icons are buttons
  (`timeline.zoomOutButton` / `timeline.zoomInButton`); a click on the slider track jumps there;
  opening the Graph Editor or switching value/speed graph fits the graphs; a selected key shows its
  graph even when its layer isn't selected; the AZERTY zoom keys zoom the graph.
- The timeline view stops at the comp end (a ruler scrub past the edge scrolls with the CTI); trim
  zones reach slightly outside layer bar ends and win over slipping; trimming a selected layer's
  in/out trims every selected layer in one undo step (`inDelta` / `outDelta`); a click on empty track
  area deselects layers.
- The Type tool edits the selected layer when text layers overlap (hidden layers skipped).

**UI polish**
- Search and text fields centre their text and placeholder vertically; long Project panel names end
  with an ellipsis; Reset and About… are centred on the Effect Controls header; Timeline ruler labels
  that would run under the marker bin are skipped.

**Benchmarks**
- `cargo xtask bench`: generated scenarios (demo comp; 1080p with 100 layers and effects; precomps
  three deep; PNG-sequence, H.264 and 4K footage) rendered at Full and Half, recording peak memory,
  CPU and wall time in `target/bench/results.jsonl`; `--compare` an earlier run and `--fail-over PCT`
  to gate a change.

## Why
Closer After Effects parity in daily workflows (Figma hand-off, importing a project into a project,
applying effects, relinking moved projects, shape fill/stroke and roundness, timeline trimming, AZERTY keyboards, Graph Editor zoom), measured fidelity fixes (particle and Echo scenes
against After Effects frames: mean absolute error 0.1855 → 0.0071 on Echo-after-particles), and
benchmark numbers tracked over time as docs/gaps.md asks.

## How tested
- `rustfmt --edition 2024 --check` on every changed `.rs` file: clean.
- `cargo clippy --all-targets -- -D warnings` for automation, effects, engine, host, project, render,
  script, svg, ui-egui, effectcraft, effectcraft-cli and xtask: clean.
- `cargo check --target wasm32-unknown-unknown` for effects, engine, project, render, svg and
  script: clean.
- `cargo test` for the same twelve packages: 1,957 passed. The menu-translation test found the
  Spanish and Portuguese catalogues missing three new entries; the last commit adds them, and the
  ui-egui lib tests pass again.
- Known failure, also on `main`: `theme::script_font_tests::arabic_words_are_joined_and_read_right_to_left`.
- Not tested end to end: a live Figma REST import (needs a token and network).
- Not run: the full `cargo xtask ci`. Tests ran on a 4-core Linux runner without a GPU (wgpu on
  llvmpipe).

Cherry-picked with `-x` trailers from topic branches already adapted to current `main`.
