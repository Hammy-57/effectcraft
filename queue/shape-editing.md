title: feat(shapes): Reverse Path Direction, Round Corners, Layer Contents in Properties, per-shape SVG rasterising
branch: upstream/shape-editing
depends: none

## What
Shape and Properties editing:

- **Reverse Path Direction** (⇄) on Path / Rectangle / Ellipse / Polystar rows in the Timeline
  (`shape.reversePathDirection {layer?, prop?, reversed?}`, one undo step; Path Direction stays
  hidden as a row). Scripting can set `ADBE Vector Shape Direction` with After Effects' values (1
  Default, 3 Reversed) and reads them back the same way.
- **Round Corners**: `layer.roundCorners` (a solid becomes a rounded shape layer; a shape
  rectangle gets roundness), Layer ▸ Create ▸ Round Corners, roundness on new shapes, an MCP test;
  translated in all ten menu catalogues.
- **Properties panel**: a Layer Contents section for shape layers and a Roundness row for solids;
  long row labels push their values right instead of overlapping them.
- **SVG**: each shape rasterises only inside its own bounds instead of a canvas-sized mask (an
  Illustrator layer went from 556 s to 5–7 s a frame); pixels match except a few anti-aliased edge
  pixels; a test checks coverage stays inside the rectangle.

Left out of #858's version: "toolbar Fill/Stroke kinds apply to selected shape layers", since
#732 (merged) already does that. Its blend-mode / opacity part isn't carried over; we can send it
on top of #732 separately if you want it.

## Why
After Effects parity for shape editing, and the SVG rasteriser cost (shapes × canvas area). Split
out of #858 as asked.

## How tested
- Tests in `engine/src/tests_tail.rs`, `script/src/tests.rs`, `ui-egui/tests/ui_timeline_shapes.rs`,
  `ui-egui/tests/ui_properties.rs`, `automation/src/tests.rs`, `svg/src/tests.rs`; written with the
  changes, not re-run on this branch.
- `rustfmt --check` and `cargo check --all-targets` on the affected crates on current upstream
  `main` (the new tests were appended next to tests added upstream since).
