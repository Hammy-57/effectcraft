title: feat(graph-editor): shared timeline time zoom, fit on open, working zoom buttons and slider
branch: upstream/feat-graph-editor-zoom
depends: upstream/feat-shortcuts-azerty

## What
- The layer bars and the Graph Editor share one time zoom, as in After Effects. Switching views keeps the zoom.
- The Timeline footer's zoom-out / zoom-in icons were painted but did nothing; they're now buttons (`timeline.zoomOutButton` / `timeline.zoomInButton`).
- A click on the zoom slider's track jumps the zoom there. Before, the slider only reacted to drags, and it didn't apply the whole-comp clamp.
- Opening the Graph Editor frames the graphs: Fit Selection to View when keys are selected, else Fit All Graphs to View. Switching between value and speed graph refits too.
- A selected key shows its property's graph even when its layer isn't selected.
- Fit goes through the timeline zoom, and the AZERTY zoom keys (`Physical+6` / `Physical+Minus`) zoom the graph.

## Why
Matches After Effects. Before this, the Graph Editor opened on an arbitrary range and kept its own zoom.

## Depends on
`upstream/feat-shortcuts-azerty`, for the `Physical+` bindings that the AZERTY zoom test uses.

## How tested
- `ui_time_zoom_controls` and `ui_graph_fit` drive `=` / `-`, the buttons, the slider and the navigator over the control channel in both views.
- clippy `-D warnings` and rustfmt pass on this branch.
