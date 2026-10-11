title: feat(shortcuts): physical-key bindings for AZERTY layouts; Go to Work Area Start/End
branch: upstream/feat-shortcuts-azerty
depends: none

## What
- Shortcut presets accept `Physical+<key>`, using egui's US-position key names. Layouts that report the same logical key for two physical keys (AZERTY's `-` and `)`) can then bind each one separately. A physical binding wins for that key press.
- When a focused panel's binding and an application command share a chord, the panel's binding wins.
- `ui.key` on the control channel accepts the same notation.
- New commands: `time.workAreaStart` / `time.workAreaEnd` (Go to Work Area Start / End), bound to Shift+Home / Shift+End as in After Effects.

## Why
On AZERTY keyboards the timeline zoom keys and other shortcuts based on key position couldn't be bound. The work-area navigation commands were missing.

## How tested
- `ui_azerty_shortcuts` (egui_kittest) and the engine test `work_area_navigation_has_after_effects_default_keys`.
- `cargo clippy -p effectcraft-engine -p effectcraft-ui-egui --all-targets -- -D warnings` and rustfmt pass on this branch.
