title: fix(shortcuts): physical-key bindings for AZERTY layouts; Go to Work Area Start/End
branch: upstream/fix-shortcuts-physical-keys
depends: none

## What
- Shortcut presets accept `Physical+<key>` (egui US-position key names). A physical binding wins for that key press, so layouts that report the same logical key for two physical keys (AZERTY's `-` and `)`) can bind them separately. `ui.key` on the control channel accepts the same notation.
- When a focused panel's binding and an application command share a chord, the panel's wins.
- New commands `time.workAreaStart` / `time.workAreaEnd` (Go to Work Area Start / End), unbound by default.

## Why
On AZERTY keyboards two physical keys arrive as the same logical key, so Zoom In/Out (and similar pairs) could not both be bound; a single press dispatched the wrong command.

## How tested
- New `crates/ui-egui/tests/ui_azerty_shortcuts.rs` (4 tests: physical vs logical dispatch, no double dispatch).
- On this branch (upstream/main + this commit): `cargo clippy -p effectcraft-engine -p effectcraft-ui-egui --all-targets -- -D warnings`; `cargo test -p effectcraft-engine shortcut`; `cargo test -p effectcraft-ui-egui --lib -- i18n menus shortcut` (33 passed); `--test ui_azerty_shortcuts` (4 passed); rustfmt --check.
