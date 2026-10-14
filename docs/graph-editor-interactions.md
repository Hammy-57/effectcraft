# Graph Editor interactions

Implementation notes for the graph-editor UX update and its follow-up. The original update's
compilation checks and focused tests ran on 2026-10-14; follow-up verification is recorded
separately below. The desktop app has not been built or launched for these changes; this document
does not claim measured After Effects parity.

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


## Follow-up: four graph gaps (2026-10-14)

- **Expression speed:** the speed curve differentiates evaluated expression values, using the
  renderer's evaluation/fallback path. Spatial properties show vector magnitude; other numeric
  properties show the selected component's speed. Disabled expressions still use the existing
  keyframe derivative. The centered difference samples 0.0001 layer-seconds on either side,
  converted to comp time through the layer start/stretch, preserving the existing velocity
  dialog's **units per layer-second** convention. The plot has at most 2,048 samples per curve,
  cached until project/view/selection changes; nonfinite derivatives become zero. As with any
  numerical derivative, discontinuities and rapidly varying expressions are approximations.
  Keys and ease handles continue to edit the underlying animation, not the expression result.
  A pinned combined Position after Separate Dimensions reads its X/Y/Z followers through the
  same evaluation path, including keyframed followers without an expression host. Its obsolete
  pre-separation keys/handles are hidden; edit the follower properties instead. Fit uses follower
  key times and includes the layer's active interval when a follower has an enabled expression.
- **Fit All:** frames the shown properties' key times, with padding, instead of always framing
  the entire composition. Expressions also include their layer's active interval, since they can
  animate beyond base keys. Empty/static graphs fall back to the composition; a lone key gets a
  bounded visible span. Fit Selection and restoration of the previous timeline zoom remain.
- **Direct key editing:** right-click offers value, interpolation, velocity, Hold, ease and delete
  commands. Right-clicking an unselected key selects it; an already selected key preserves the
  multi-key selection for the command. Double-click opens Edit Value for scalar/vector values
  in Value mode and Keyframe Velocity in Speed mode; types without a value dialog open
  Interpolation instead. All edits use existing commands/dialogs and undo.
- **Set isolation:** Properties > Graph Editor Set Only shows only pinned numeric properties,
  including an empty view when no properties are pinned. Choosing Selected/Animated exits
  isolation; the older additive Show Graph Editor Set option remains. This is an EffectCraft
  convenience; exact After Effects set-only toggle behavior has not been verified. The setting
  is serialized with timeline UI state and defaults off for older settings.

Follow-up regression coverage is in `ui_graph_fit` and `panels::graph::tests`: expression
scalar/vector results, error fallback, disabled expressions, stretched/reversed/offset layers,
nonfinite outputs, fitting, context selection/undo/dialogs, and set isolation/serialization.
`cargo check -p effectcraft-ui-egui --all-targets` and
`cargo clippy -p effectcraft-ui-egui --all-targets -- -D warnings` both passed in the isolated
lane with one compile job, as did formatting and `git diff --check`. These nine newly written
tests and the two updated fit assertions have not been executed under the fork's daytime
policy; the earlier 83-test result above covers only the preceding graph-editor change.
No live app or After Effects comparison was performed. Run `ui_graph_fit::` in the UI test
binary and `panels::graph::tests::` in the library test binary during the permitted test window.


Review also covered the separated-Position aggregate: the added regression pins Position before
separating, leaves obsolete combined keys in the document, then checks follower expressions,
follower-only keyframes without an expression host, matching Fit bounds and the absence of
obsolete key handles. Two earlier graph-update cloud regressions were repaired: the timeline
pin tooltip now uses the UI translation catalog, and the transform-box regression opens the
current graph-type menu to access Reference. Those runtime tests remain deferred locally;
compilation/lint checks do not establish that the cloud test suite has passed after the repair.

## Combined integration checks — 2026-10-14

On the integrated interaction branch, the render/engine/UI all-targets native check and strict
Clippy passed. The focused `wasm32-unknown-unknown` render/engine/UI check also passed, with warnings
in unchanged GPU/readback, frames, pointer-wrap and zero-copy code. Formatting and diff checks passed.
Tests were compiled, not executed, under the daytime policy. No full CI, native application rebuild,
restart, upstream push or PR was performed; native/browser runtime acceptance remains pending.


## Before/after expression comparison (2026-10-14)

Graph type/options > **Compare Before/After Expression** adds a dim base curve behind the bright
evaluated result, on the same axis. The two-row legend identifies the phases, and hovering either
curve identifies its phase and value. The option defaults off (including older saved UI state),
so existing users retain the evaluated-result view until they choose comparison.

This implements the graph comparison described in Adobe's public
[Editing expressions documentation](https://helpx.adobe.com/after-effects/desktop/work-with-expressions/edit-expressions/edit-expressions.html):
base animation is dim and the evaluated result is bright. It is independent of **Show Reference
Graph**, which continues to display the other graph type (value versus speed) on its own scale.
Composition-viewer motion-path overlays and the Graph Editor expression text editor are outside
this change; no Adobe application internals or assets were used.

- Base samples evaluate with the expression host omitted. Separated Position still combines its
  follower values, so comparison works for follower expressions as well as ordinary properties.
- Disabled expressions do not produce a redundant comparison pair. Expression errors follow the
  renderer's existing fallback; identical base/result lines naturally overlap. Keys and handles
  continue to edit base animation; evaluated curves themselves are readouts.
- Auto-height and Fit include both sample lists when comparison is enabled. Manual zoom is
  respected. Speed keeps the existing units per layer-second. The centered numerical derivative
  approximates discontinuities with finite spikes; both phases use the same stencil so identity
  expressions and error fallbacks do not invent a difference. The graph is not an analytic
  expression solver.
- Samples remain bounded to 2,048 points per curve. The cache stores paired lists only when needed
  and keys them by comparison state and expression-host availability as well as the existing
  document/view inputs. Warm frames reuse both lists. The opposite-type Reference Graph does not
  allocate an unused base list. This is a correctness extension, not a measured performance claim.
- `graph::reset_context` cancels old graph gestures and requests Fit when opening a graph or
  switching its source composition. The inline graph caller uses the same entry point.

Eight new overlay regressions use a fake expression host for paired values, warm cache reuse,
host removal, disabled expressions, identity/error speed at smooth segments and Hold jumps,
separated Position, stretch, discontinuities, base-curve extrema in Fit, independent controls,
read-only behavior and serialization. A separate regression covers graph-context gesture reset.

Verification for this follow-up: scoped `cargo check -p effectcraft-ui-egui --all-targets` passed,
and final-source `cargo clippy -p effectcraft-ui-egui --all-targets -- -D warnings`, formatting and
`git diff --check` passed, with one compile job. Source review caught and corrected a mismatch
between the before/after derivative stencils; re-review found no remaining blocker. The new
regressions compiled but were not executed under the fork's daytime policy. No live app/reference
comparison, screenshot verification or performance benchmark was performed for this change.


## Narrow invalidation for plain graph samples (2026-10-14)

The measured unrelated-edit workload motivated a targeted cache change: when every shown
property is free of active expressions, samples depend on the actual owning layer snapshots
rather than the whole Session revision. The cache retains those `Arc<Layer>` handles from
`comp.layers.arcs()` and compares them with `Arc::ptr_eq`; it never hashes a borrowed/raw address.
Copy-on-write edits therefore invalidate the affected owner even if an outer cursor's revision
did not change. Retention also prevents an allocator from recycling an old owner's address into
a false hit. Active expressions, including separated Position followers, retain conservative
whole-Session revision invalidation because they may read any layer or composition.

Shown properties, composition identity, graph type, view geometry, expression-host availability
and comparison state still participate in validity. On a warm hit, the returned curves clone the
cached points once; the unused second point-vector copy is gone. Key selection/handle geometry
is still rebuilt from current state. The sampler/show entry points remain unchanged for A/B
probe replay.

This deliberately caches at layer granularity: another property edit on the same shown layer
still invalidates. Each value/speed cache retains its displayed owner layers until replacement
or context destruction, even while the graph is hidden; duplicate handles do not duplicate
layer data. Expression hosts are assumed stable while present, as before; replacing one live
host with another must also change the Session revision.

Five new regressions cover unrelated-edit reuse, real owner retention, key/value/stretch/3D
changes, separated-follower COW edits without a revision change, enabled expressions and global
composition dependencies, and view/type/comp changes. Source review found no blocker. Scoped
`cargo check -p effectcraft-ui-egui --all-targets`, final-source
`cargo clippy -p effectcraft-ui-egui --all-targets -- -D warnings`, formatting and diff checks
passed with one compile job. The new regressions compiled but were not executed under daytime
policy. Parent A/B timing of the candidate remains separate; this note does not claim a measured
speedup or an input-to-visible latency result.
