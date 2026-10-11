title: fix(memory): release the old document's layer and GPU caches when a project is replaced (#591)
branch: upstream/fix-release-caches-on-open
depends: none

## What
`Session::replace_project` (open, new) now clears the layer cache and sends `Event::PurgeCaches`; the app's `PurgeCaches` handler also clears the GPU upload cache (`clear_uploads`), not only the frame cache. Edit ▸ Purge ▸ Memory gets the GPU clear too.

## Why
Part of #591 (GPU allocation failures): opening a project kept the previous document's rendered layer buffers and GPU uploads alive until they were evicted, so memory/VRAM from a closed document competed with the new one.

## How tested
- New engine test `replacing_project_releases_old_layer_buffers_and_notifies_viewers`.
- On this branch: `cargo clippy -p effectcraft-engine -p effectcraft-ui-egui --all-targets -- -D warnings`; `cargo test -p effectcraft-engine --lib -- replacing_project purge`; `cargo test -p effectcraft-ui-egui --lib gpu_failure`; rustfmt --check.
