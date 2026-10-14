# Graph Editor interactions

Implementation notes for the graph-editor UX update. Compilation checks and focused tests were authorized and run on 2026-10-14. The desktop app
has not been built or launched; this document does not claim measured After Effects parity.

- **Graph type:** new preferences default to Auto (spatial properties use Speed, other properties
  use Value). Previously saved explicit Speed/Value choices stay unchanged. The type menu shows
  the resolved type and contains the reference-graph toggle.
- **Navigation:** Hand tool, held Space or middle-button drag pans the graph. Wheel pans values;
  Shift-wheel pans time; Option/Alt-wheel zooms time; Command/Ctrl-wheel zooms values. Zoom tool
  clicks zoom time (Option/Alt-click zooms out); dragging zooms time, or values with Option/Alt
  and a predominantly vertical drag. Auto-zoom height disables manual vertical pan/zoom.
- **Handles:** Option/Alt-drag splits a continuous handle pair. For a split pair, dragging to
  the opposite side's speed rejoins it. Influence remains independent on each side. With
  auto-height enabled, pulling a handle beyond the plot expands the displayed range; the
  gesture retains its original coordinate scale to avoid feedback. Small handles keep a 12-point
  minimum grip distance; manually clipped handles remain reachable at the plot edge.
- **Snapping:** keys and transform-box drags align to other unselected keys, time markers,
  layer endpoints, work-area and composition boundaries. Value alignment is available where
  values are editable. Orange lines identify the alignment; Command/Ctrl temporarily reverses
  the Snap toggle during a drag. Transform-box movement snaps its original lower-left bound.
- **Readouts:** curves and keys show property, component, time and value; handles show speed and
  influence. Axis labels include units, and the reference graph has a right-hand numeric scale.
- **Graph Editor Set:** toggle the graph switch beside a property's stopwatch, or use its context
  menu. Pins save in the project and undo normally. Properties > Show Graph Editor Set adds
  pinned curves regardless of layer selection.
- **Toolbar:** graph and property-display settings are grouped in menus. Editing tools use
  compact original icons or short labels with full tooltips; narrow panels retain the overflow
  menu. Optional owner-local artwork continues to load through the existing asset mechanism.

## Verification (2026-10-14)

- `cargo check -p effectcraft-project -p effectcraft-engine -p effectcraft-ui-egui --all-targets` passed.
- `cargo test -p effectcraft-ui-egui --test ui -- ui_graph ui_keyframes:: --test-threads=2`:
  **31 passed**. Covers graph fit, saved set visibility, graph type menus, narrow-toolbar overflow,
  Hand/middle/Space panning without key edits or playback, wheel navigation, value snapping and
  its modifier override, handle split/join with undo, and auto-height expansion.
- `cargo test -p effectcraft-engine -p effectcraft-ui-egui --lib -- tests_timeline:: panels::graph:: panels::graph_tools:: --test-threads=2`:
  **48 engine + 4 graph utility tests passed**, including pin serialization/legacy defaults/undo
  and continuous handle edits preserving the opposite influence.
- `cargo clippy -p effectcraft-project -p effectcraft-engine -p effectcraft-ui-egui --all-targets -- -D warnings` passed.
- Formatting and `git diff --check` passed.

Testing found and fixed missing split/join fields in the handle-drag state, an outdated synthetic
wheel-event initializer, vertical panning being overwritten by an idle wheel handler, and middle
presses missing navigation ownership on their first frame. Toolbar tests now target the grouped
menus and exercise a deliberately narrow panel.

Still pending: desktop app build/launch, live visual review (including reference-scale readability),
and direct After Effects interaction comparisons. No full CI or release build was run.
