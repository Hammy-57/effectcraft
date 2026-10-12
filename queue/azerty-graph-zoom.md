title: feat(shortcuts): AZERTY physical-key bindings, Go to Work Area keys, Graph Editor zoom and fit
branch: upstream/azerty-graph-zoom
depends: none

## What
Keyboard layouts and time zoom:

- **Physical-key shortcuts**: presets accept `Physical+<key>` (egui's US-position names), so
  layouts that report the same logical key for two physical keys (AZERTY's `-` and `)`) can bind
  them separately; a physical binding wins for that press, and a focused panel's binding wins over
  an application command on the same chord. `ui.key` accepts the same notation.
- **Go to Work Area Start / End** (`time.workAreaStart` / `time.workAreaEnd`), bound to Shift+Home /
  Shift+End as in After Effects.
- **Graph Editor zoom**: the layer bars and the Graph Editor share one time zoom; the footer's
  zoom-out / zoom-in icons are buttons and a click on the zoom slider jumps; opening the Graph
  Editor fits the selection (or all graphs), and the fit goes through the timeline zoom; AZERTY zoom
  keys work in the graph.
- **View stops at the comp end**; a ruler scrub past the edge scrolls with the CTI.

## Why
On AZERTY keyboards several of After Effects' default shortcuts couldn't be reached or collided;
the Graph Editor had its own zoom. Split out of #858 as asked.

## How tested
- `ui-egui/tests/ui_azerty_shortcuts.rs`, `ui_time_zoom_controls.rs`, `ui_graph_fit.rs`,
  `ui_keyframes.rs`, engine `tests_tail.rs`; written with the changes, not re-run on this branch.
- `rustfmt --check` and `cargo check --all-targets` on the affected crates on current upstream
  `main` (`menus.rs` adapted to the SVG-paste handling added upstream since).
