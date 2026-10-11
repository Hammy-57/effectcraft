title: feat(timeline): trim zones past bar ends, multi-layer trim, click-to-deselect, view stops at comp end
branch: upstream/feat-timeline-trim-select
depends: upstream/feat-graph-editor-zoom

## What
- The timeline view stops at the comp end. A ruler scrub past the edge scrolls with the CTI.
- Layer bar trim zones reach slightly outside each end (4 pt outside, up to 5 pt inside), and trimming wins over slipping there. Before, just past a footage layer's end the pointer slipped the source instead of trimming.
- Trimming a selected layer's in/out point trims every selected layer by the same amount, each within its own limits, in one undo step (`inDelta` / `outDelta`).
- A plain click on empty track area deselects layers. A marquee that catches nothing leaves nothing selected. Shift / Cmd keep the selection.

## Why
All of these match After Effects.

## Depends on
`upstream/feat-graph-editor-zoom`: the view-end clamp builds on the shared time zoom and its tests.

## How tested
- `ui_timeline_trim`, `ui_time_zoom_controls` and `ui_workflow_selection` (egui_kittest); engine `tests_timeline` (`timing_deltas_trim_each_layer_within_its_limits`).
- clippy `-D warnings` and rustfmt pass on this branch.
