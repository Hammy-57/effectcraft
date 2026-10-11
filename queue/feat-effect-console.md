title: feat(ui): Effect Console (Ctrl+Space) to search and apply effects at the pointer
branch: upstream/feat-effect-console
depends: none

## What
Effect Console (Edit ▸ Effect Console…, Ctrl+Space; control channel `app.fxConsole {query?}`): a small search bar at the pointer. It ranks effects, text presets and user presets by name, word starts, initials (`gb` = Gaussian Blur) and letters in order. Recent and favourite effects rank higher and Obsolete ones lower. Enter applies to the selected layers and closes; Shift+Enter applies and stays open; the arrows choose; Escape, Ctrl+Space or a click outside closes. With no text, it lists recent effects.

## Why
It's a faster way to apply effects than browsing Effects & Presets, and it's one of the most requested workflow tools in After Effects.

## How tested
- `ui_fx_console` (egui_kittest) covers ranking, applying, Shift+Enter and closing.
- `cargo clippy -p effectcraft-engine -p effectcraft-ui-egui --all-targets -- -D warnings` and rustfmt pass on this branch.
- Not checked: UI fingerprints. They're enforced only on aarch64 macOS, and this ran on Linux.
