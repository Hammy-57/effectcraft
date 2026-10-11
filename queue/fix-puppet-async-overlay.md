title: fix(puppet): solve the Puppet overlay off the UI thread and reuse mesh solves (#586)
branch: upstream/fix-puppet-async-overlay
depends: none

## What
Puppet tool responsiveness (#586):
- **Effects:** the Puppet mesh solve is reused when only unrelated inputs change, and a mesh cache hit no longer allocates the alpha buffer.
- **Viewer:** a changing Puppet overlay (dragging a pin, scrubbing) is solved off the UI thread through a bounded mailbox on the existing frame pool: only the running pose and the latest requested pose hold project snapshots, intermediate poses coalesce, and a failed or panicking solve shows a status message instead of taking the app down.
- **Engine:** `Session.epoch` counts project replacements (`replace_project`), so the overlay cache can't show a stale mesh after opening another project whose comp/layer ids match.

## Why
Dragging a Puppet pin solved the mesh synchronously on the UI thread every frame, stalling the viewer on large layers (#586).

## How tested
- On this branch (upstream/main + these commits): `cargo clippy -p effectcraft-engine -p effectcraft-effects -p effectcraft-ui-egui --all-targets -- -D warnings`; `cargo test -p effectcraft-effects --lib puppet` and `cargo test -p effectcraft-ui-egui --lib puppet` (mailbox coalescing / worker exit tests); rustfmt --check.
