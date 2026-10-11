title: perf(3d): depth of field gathers only the bracketing blur levels; skip unused shadow casters
branch: upstream/perf-3d-dof-shadows
depends: none

## What
- 3D depth of field: `progressive_blur` blurred the whole image at up to eight radii and kept them all. The iris kernels now gather only the two levels around each pixel's radius from one set of row prefix sums. Fast Rectangle's O(1) box levels are unchanged.
- Both 3D compositors skip preparing out-of-run shadow casters when `RenderOpts::shadows()` is false (Draft without Realtime Shadows), since those casters were dropped unused.

## Why
Depth of field is about 4× faster at 640×480 with 0–24 px radii. Draft 3D renders no longer pay for shadow casters they never draw.

## How tested
- The DOF change is checked bit-exact against the old code, which is kept as a test oracle. The shadow change leaves the output unchanged.
- `cargo clippy -p effectcraft-render --all-targets -D warnings` and `cargo test -p effectcraft-render` (render goldens and oracles included) pass on this branch on top of `main`.
