title: perf(svg): rasterise each shape within its own bounds
branch: upstream/feat-svg-shape-bounds
depends: none

## What
The SVG rasteriser paints each fill and stroke only inside its own device-space bounding box, plus 1 px for anti-aliasing and clipped to the canvas. A stroke uses the bounds of its outline. Before this, each one allocated and painted a coverage mask the size of the whole canvas.

## Why
A vector file cost (number of shapes) × (canvas area). One 4001×2501 Illustrator layer with many shapes took about 556 s per frame. It now takes 5–7 s. The pixels match the old whole-canvas output except for a few anti-aliased edge pixels, which differ by float rounding of the re-based transform.

## How tested
- A new test checks that every pixel a shape covers lies inside its rectangle, and pins the output.
- `cargo test -p effectcraft-svg`, clippy `-D warnings` and rustfmt pass on this branch.
