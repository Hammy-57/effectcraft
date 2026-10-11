title: feat(shapes): toolbar Fill/Stroke apply to selected shape layers; Reverse Path Direction
branch: upstream/feat-shapes-fill-stroke-reverse-path
depends: none

## What
- **Fill/Stroke from the toolbar apply to selected shape layers.** Before, they only changed the shape tools' defaults. `shape.fillStroke` now takes `fillType` / `strokeType` (`none|solid|linear|radial`), `fillBlend` / `strokeBlend` and `fillOpacity` / `strokeOpacity`. It applies them to every Fill/Stroke and Gradient Fill/Stroke in the layers' contents, including nested groups, in one undo step:
  - None switches the items off; they're kept, as in After Effects.
  - Linear / Radial convert in place and keep the shared properties.
- **Reverse Path Direction (⇄)** on Path, Rectangle, Ellipse and Polystar rows in the Timeline: `shape.reversePathDirection {layer?, prop?, reversed?}`, one undo step. Path Direction stays hidden as a row. Scripts can set `ADBE Vector Shape Direction` with After Effects' values (1 Default, 3 Reversed).

## Why
Matches After Effects. Before, the toolbar's Fill: None left a selected layer's fill in place.

## How tested
- `ui_timeline_shapes` (egui_kittest); engine `tests_tail` (fill/stroke kinds, reverse path); script `path_direction_uses_after_effects_values`.
- clippy `-D warnings` and rustfmt pass on this branch.
- Upstream port: the fork's After Effects-import mapping and the work-area keys are left out. The keys are in `feat-shortcuts-azerty`.
