# Mask, shape and panel-window interactions

Implemented 2026-10-14. This work completes the concrete interaction findings in the local audit;
it does not add arbitrary render-stage ordering or per-item preview/render modes.

## Mask and shape editing

- With editable path vertices showing, an empty-area Selection-tool drag selects vertices even
  over a full-frame layer. Shift adds points; an empty non-Shift marquee clears point selection.
  Alt-drag remains available for moving the layer; vertex and transform-handle hits retain priority.
- Arrow/Shift-arrow nudges selected vertices at the current viewer magnification before nudging
  layers. Each nudge is undoable. Multi-path drags convert each selected point's displacement through
  that path's own transform, including 3D perspective; they do not reuse the grabbed path's scale.
  Commands resolve the current view camera without depending on a prior viewer paint.
- Mask locks are enforced in point mutation commands as well as the viewer. Locked outlines may
  remain visible, but do not expose draggable vertices/tangents/feather handles. A stale mixed
  selection containing a locked mask is rejected atomically.
- Dragging a vertex or tangent (including a newly placed Pen vertex) at the viewer edge pans while
  held. Speed and timestep are bounded; releasing the pointer stops the auto-pan.
- Shape groups, paths, paints and operators expose their existing enabled flag in the Timeline.
  Fixed settings groups such as Dashes/Taper/Wave do not get misleading eye buttons. Normal,
  search and revealed property rows share the same eligibility rule.
- Convert To Bezier Path handles Rectangle, Ellipse and Polystar using the renderer's evaluated
  primitive geometry. Group identity/name, enable state, label, direction, ordering and surrounding
  transforms/paints stay intact. Undo restores the original parametric item. The item row has a
  context-menu entry; the existing Layer command also works.

`path.convertToBezier {layer?, prop?: primitive_uid, snapshot?}` converts a selected primitive
or the primitive items of selected shape layers. The existing mask/RotoBezier behavior remains.
Animated or expression-driven primitive parameters are refused unless `snapshot:true` explicitly
requests current-frame conversion; this prevents silent animation loss. Path Direction remains
an independent retained property, including its existing keys. This is not automatic animation baking.

`mask.moveVertices` keeps its existing `delta:[dx,dy]` path-space parameter. Optional
`deltas:[{layer,mask,index?,delta}]` supplies a per-path or per-vertex displacement for a shared
undo transaction. When provided, it must cover every selected vertex/path. Values are bounded
and finite, duplicate entries are refused, and missing/locked targets cannot partially mutate
the selection. The UI uses per-vertex entries for perspective correctness.

## Native panel windows

Ordinary undocking and `window.panelWindow` now use the same native floating-group implementation.
This shares shortcut handling, tab dragging, viewport resource protection, close behavior and
screen-position persistence. `window.panelWindow {window:false}` docks the group back into the
main window instead of replacing one native floating window with another.

Old serialized `panel_windows` records remain readable. Their authoritative screen rectangles are
migrated into the matching current floating groups and current named workspace; other named
workspace placements are preserved. New windows store placement in `Floating.screen`. Workspace
switches reset transient viewport placement so saved coordinates can take effect.

The web/headless fallback still embeds floating panels inside its one surface. A second video
preview window is separate from dedicated video-output hardware support.

## Regression coverage and verification

Written coverage includes:

- Full-frame marquee and vertex nudge without moving the layer; locked masks have no edit targets.
- Held edge-drag auto-pan stops on release.
- Differently scaled layers and a tilted 3D mask move by equal screen distances, even with a stale
  last-painted camera.
- Per-path/per-vertex displacement is one undo; missing deltas and locked edits fail atomically.
- Primitive geometry and identity survive conversion; undo and explicit animated snapshot policy.
- Shape Fill/Stroke visibility, no eye on Dashes, and filtered rows retain the correct eye switches.
- Menu window round trips, legacy placement migration and named workspace switching.

Scoped `cargo check -p effectcraft-render -p effectcraft-engine -p effectcraft-ui-egui --all-targets`
and strict Clippy for the same targets passed on the isolated interaction branch. Formatting and
diff checks passed. Test execution is deferred
under the fork's daytime build policy; compiling tests does not establish that they pass.
Native focus, monitor unplugging, mixed-DPI dragging, close behavior and visual feel still need a
controlled runtime pass. No existing app or clipboard session is used by this implementation work.

## Combined integration checks — 2026-10-14

On the integrated interaction branch, the render/engine/UI all-targets native check and strict
Clippy passed. The focused `wasm32-unknown-unknown` render/engine/UI check also passed, with warnings
in unchanged GPU/readback, frames, pointer-wrap and zero-copy code. Formatting and diff checks passed.
Tests were compiled, not executed, under the daytime policy. No full CI, native application rebuild,
restart, upstream push or PR was performed; native/browser runtime acceptance remains pending.
