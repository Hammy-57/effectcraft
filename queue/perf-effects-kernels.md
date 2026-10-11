title: perf(effects): faster transpose, Median, Camera Lens Blur and Cell Pattern (bit-identical)
branch: upstream/perf-effects-kernels
depends: none

## What
Four CPU effect kernels do less work per pixel, with bit-identical output:
- `Plane::transpose` is cache-blocked in 32×32 tiles. It runs twice per vertical pass of `box_plane`/`gauss_plane`.
- Median updates its histogram inline through a generic column visitor instead of a per-sample `&mut dyn FnMut`.
- Camera Lens Blur sums the iris span counts once per iris instead of once per output pixel.
- Cell Pattern computes each cell's feature point once per frame instead of once per sample.

## Why
These loops dominated the run time of these effects. Cell Pattern at High Quality is about 7× faster.

## How tested
- Each change keeps the old code as a test oracle and checks exact (`f32` bit) equality: the column-by-column transpose, the old Median and the per-sample Cell Pattern computation. Lens Blur's output is unchanged.
- On this branch on top of `main`: `cargo clippy -p effectcraft-effects --all-targets -- -D warnings`, `cargo test -p effectcraft-effects` (all pass) and rustfmt.
