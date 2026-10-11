title: perf(memory): GPU upload cache, Roto Brush cache and disk-cache queue follow the memory budget
branch: upstream/perf-memory-budgets
depends: none

## What
More caches follow the Memory & CPU budget instead of fixed sizes:
- GPU: the upload cache drops the textures of freed layer buffers on every upload, not only once it is over budget. When over budget, it evicts the least recently used uploads. The upload cache and the texture pool take their budgets from the Memory & CPU layer-cache budget, through the UI.
- Roto Brush segmentations are cached up to half the layer-cache budget (at most 768 MB) instead of a fixed size.
- The disk-cache write queue is bounded by bytes, not by a frame count.

## Why
Dead textures stayed resident until the fixed upload budget was exceeded, and these caches ignored the memory the user set aside.

## How tested
- On this branch on top of `main`: `cargo clippy --all-targets -- -D warnings` and `cargo test` (`--lib` for ui-egui) pass for `effectcraft-gpu`, `effectcraft-effects`, `effectcraft-render`, `effectcraft-engine` and `effectcraft-ui-egui`, and rustfmt is clean. Two GPU tests (`tests_fx_pixel2::stylize`, `tests_fx_stylize::posterize_thresholds_strobe_vignette`, llvmpipe) and `theme::script_font_tests::arabic_words_are_joined_and_read_right_to_left` fail on `main` in this Linux environment too.
