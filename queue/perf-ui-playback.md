title: perf(ui): no frame copies or display-rate repaints during playback
branch: upstream/perf-ui-playback
depends: none

## What
- Viewers upload shared frames without copying their pixels. Passive viewers share the comp instead of cloning it every frame.
- A playing preview repaints when its next frame is due, not at the display's refresh rate.
- The viewer's channel and exposure transform runs in parallel.

## Why
During playback the UI copied every frame and repainted at up to 120 Hz for 24–30 fps content. That left less CPU for rendering.

## How tested
- On this branch on top of `main`: `cargo clippy -p effectcraft-ui-egui --all-targets -- -D warnings` and `cargo test -p effectcraft-ui-egui` pass, including every ui-egui integration test binary, and rustfmt is clean. Two GPU tests (`tests_fx_pixel2::stylize`, `tests_fx_stylize::posterize_thresholds_strobe_vignette`, llvmpipe) and `theme::script_font_tests::arabic_words_are_joined_and_read_right_to_left` fail on `main` in this Linux environment too.

Ported from the fork. The last commit is rustfmt.
