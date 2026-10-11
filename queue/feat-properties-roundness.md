title: feat(properties): Roundness for rectangles and solids; layer.roundCorners; Layer Contents section
branch: upstream/feat-properties-roundness
depends: none

## What
- **Properties panel:** shape layers get a Layer Contents section that lists their paths, Fill and Stroke with automation ids. Solids get a Roundness row: scrubbing or typing a value turns the solid into a rounded-rectangle shape layer, in one undo step (also when scrubbed). Long row labels push their values right instead of overlapping them.
- **`layer.roundCorners {layer?, roundness}`** turns a solid into a rounded shape layer, or sets the Roundness of a shape layer's rectangles. Also available as Layer ▸ Create ▸ Round Corners. New rectangle shapes get a roundness parameter too.
- The menu entry is translated in the four catalogues.

## Why
Rounding a rectangle is a common request. In After Effects it means converting to a shape layer and finding Roundness several levels deep.

## How tested
- `ui_properties` (egui_kittest): the Roundness row and the Layer Contents section.
- An automation (MCP) test for `layer.roundCorners`.
- clippy `-D warnings` and rustfmt pass on this branch.
- Not checked: UI fingerprints. They're enforced only on aarch64 macOS.
