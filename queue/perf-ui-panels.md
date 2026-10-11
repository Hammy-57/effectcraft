title: perf(ui): panels stop rescanning, re-decoding and rebuilding every UI frame
branch: upstream/perf-ui-panels
depends: none

## What
UI panels stop redoing work on every UI frame:
- Thumbnail textures (Media Browser, Project panel, Home) are bounded by an LRU (`thumb_lru.rs`).
- The Timeline cache bar remembers its frame lists per cache generation. `cached_frames`/`disk_frames` used to scan the RAM cache and hash a disk key per comp frame, under mutex locks, on every UI frame. The RAM and disk caches now count their changes.
- Lumetri Scopes share their cached scope and read the shown frame while playing.
- The Footage panel borrows its footage and decodes on a worker thread, averaged down to the panel size, keyed on item, frame and interpretation.
- The native menu catches up after a drag instead of rebuilding on every frame of it. Its updates are paced to 4 per second and pushed only when the spec changes.

## Why
These per-frame scans, decodes and rebuilds made the UI thread the bottleneck on large projects.

## How tested
- On this branch on top of `main`: `cargo clippy --all-targets -- -D warnings` and `cargo test` pass for `effectcraft-ui-egui` (lib and every integration test binary), `effectcraft-render` and the `effectcraft` app, and rustfmt is clean. Two GPU tests (`tests_fx_pixel2::stylize`, `tests_fx_stylize::posterize_thresholds_strobe_vignette`, llvmpipe) and `theme::script_font_tests::arabic_words_are_joined_and_read_right_to_left` fail on `main` in this Linux environment too.

Ported from the fork. Two small commits declare `thumb_lru` in `lib.rs` (the fork registers modules through a build script) and fix a clippy `needless_borrow`.
