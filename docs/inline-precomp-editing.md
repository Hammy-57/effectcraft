# Inline precomposition editing

A precomp layer has a second disclosure arrow in its source-icon position. The normal arrow still
opens the instance's own properties. The second arrow expands the source composition's real layers
under the instance, indented in the parent timeline. The parent viewer stays open.

The source layers are shared, not copies. Expanding two instances displays the same source layers
at their respective instance times. Selecting a source layer/property/key is reflected in every
expanded instance. Changing it edits the source once, with one undo step; dragging or typing groups
continuous changes. Undo and redo leave the parent composition active.

Supported inline operations:

- Expand nested compositions recursively, and open source-layer property groups.
- Select source layers/properties/keys, edit numeric, vector, Boolean, enum, colour and text values,
  use the existing gradient editor, and edit expressions from a property context menu.
- Change visibility/audio/solo/lock switches, and effects, motion blur, 3D, shy and collapse switches
  from the source layer's context menu. Toggle effect, mask and shape instance groups.
- Rename or move source layers up/down from their context menu; drag their duration bars in time.
- Toggle animation, add/remove keys, Shift-select multiple keys, move selected keys, and use the
  key's context menu for ease/hold/delete. Delete and Duplicate address the source selection.
- Open the source composition explicitly from a nested row's context menu for its full editor.

## Timing and safety

`comp.inlineEdit {root?, path: [instanceLayerIds], command, params?}` is the command used by the UI.
The path is checked from the parent composition on every edit. It is bounded to 32 levels and
rejects cycles, missing sources, invalid stretch and locked ancestors. Rendering and document
structure are unchanged. The command enters the source composition only while dispatching an
approved synchronous edit, then restores the parent viewer, time, selection and text-edit context.
Source selection is stored in the existing per-composition selection map. Failed compound edits
restore the document/history; successful compound edits create one undo step even when the undo
history is full. The wrapper is conservatively excluded from atomic `engine.batch` steps.

Evaluation follows the renderer's `EvalCtx::nested_time`: offsets, positive/negative stretch, time
remapping, protected time regions and preserved source frame rate use the same source time as the
viewer. Ordinary instance bars and keys are mapped back onto the parent time ruler. For nonlinear
remapping/protected-time/preserved-frame-rate chains, the inverse can be ambiguous: inline properties
and animation switches still operate at the evaluated source time, while bars/key dragging are
replaced by a source-time label. Open the source timeline for those key-time edits.

UI expansion is instance-specific and saved with timeline preferences; keyboard focus is temporary.
The projection caps additional displayed rows at 20,000. Source-layer property expansion is shared
across instances. The parent selection is parked while inline focus is active, so its viewer handles and inspectors
do not offer edits to the wrong layers. Global selection-sensitive actions without an inline implementation are refused
while a nested row owns timeline focus, with an explanation; they never fall through to the parent
selection. Clicking a parent row or moving focus to another panel restores normal targeting.

## Remaining limitations and verification

This is an inline timeline editor, not a second source viewer. Spatial path handles, waveform
inspection, graph-editor curves for nested sources and clipboard commands still require opening
the source composition. Mixed-source selection/dragging between compositions is not supported.
The same source used at different times has one shared selection, but each instance displays its
own evaluated values.

Synthetic engine/UI regressions cover instance identity, shared selection, undo, offset/reverse
stretch and layer-time keys, remapping, frame preservation, cycles/invalid paths, locks, failed
compound-edit rollback and keyboard deletion targeting. These tests are written and compile-checked;
they have not been executed under the fork's daytime test policy. Native visual QA, interactive
nested dragging and WASM runtime verification remain pending. No full CI or running-app changes.

## Combined integration checks — 2026-10-14

On the integrated interaction branch, the render/engine/UI all-targets native check and strict
Clippy passed. The focused `wasm32-unknown-unknown` render/engine/UI check also passed, with warnings
in unchanged GPU/readback, frames, pointer-wrap and zero-copy code. Formatting and diff checks passed.
Tests were compiled, not executed, under the daytime policy. No full CI, native application rebuild,
restart, upstream push or PR was performed; native/browser runtime acceptance remains pending.
