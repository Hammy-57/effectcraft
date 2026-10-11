# User reports: 10–11 October 2026

The owner's two screenshots named fifteen bugs and three later requests. This pass covers the
bugs plus the broken Gradient Edit and empty-timeline drop workflows. In-place precomp expansion
(#601) and a whole Properties-panel parity audit remain separate feature work.

| Report | Result and regression evidence |
|---|---|
| #580 Chromatic Aberrations Pan/Tilt crash | **Still unreproduced.** The released v0.7.0 did expose these controls; this fork replaced that model earlier. Current direction controls pass CPU/GPU comparisons, but the original input, backend and crash diagnostic are missing. See [GPU evidence](gpu-580-591-613.md). |
| #591 GPU memory/device loss | GPU retention is bounded, including pending texture reuse; purge and document replacement clear CPU/GPU caches. Real compositor-device destruction preserves the document, undo and a working CPU preview/presentation in `ui_gpu::compositor_device_loss_keeps_the_window_rendering_and_the_document_editable`. The original Adreno/ARM64 project and loss of the presentation device itself remain unverified. |
| #613 DX12 compositor startup | Windows x64 links DXC. Six focused tests passed on an RTX 4060 Laptop using DX12, including actual pointwise pixels. ARM64 and x86 do not get this static compiler. See [GPU evidence](gpu-580-591-613.md). |
| #624 duplicated text cannot be edited | Reproduced with overlapping copies: the Type tool chose the top copy instead of the explicitly selected original. Selected eligible text now wins for Type clicks; double-click still chooses the topmost text. Nine `ui_text_edit` regressions pass, including undo and hidden/locked/inactive/solo exclusions. |
| #652 missing media in the app | Ordinary project opening now anchors relative footage, sequence and proxy paths to the project folder. A generated H.264/WAV project opened with app-style background checks renders both picture and audio and exports an MP4 containing both streams (`relative_media`). The reporter's actual project was not supplied. |
| #660 Windows black strip/click offset | **Left unresolved at the owner's request.** The supplied screenshot is VectorCraft; no EffectCraft reproduction was provided. See [investigation](windows-titlebar-660.md). |
| #647 drag CPU/memory growth | Urgent viewer renders no longer bypass admission limits and nest without bound. Auxiliary Puppet renders share one additional permit across layers. Queue/admission tests and `viewer-drag-memory.jsonl` cover the mechanism; the replay's native-viewer requirement is explicit. |
| #595 playback cache gaps and third-resolution indicators | Near-playhead frames are protected from speculative eviction, lookahead respects capacity, and scale hashing uses the same precision as the cache bar. Native audio now interpolates the audible sample cursor between device callbacks, holds through underrun silence, and waits for the final buffer to play. Playback samples that cursor once per scheduling decision, avoiding an extra pause at loop boundaries. Queue, eviction, scale and audio regressions pass. Smooth playback on the reporter's Linux/Wayland machine was not measured. |
| #586 Puppet drag lag | Later poses solve off the UI thread; one running/latest pose is kept, pin handles stay current, stale project results are rejected, and failures release admission. Shared deformation results also avoid duplicate solves. Initial mesh selection and the wasm path remain synchronous. Synthetic solver measurements are not a native-screen frame-rate claim. |
| #637 precomp double-click | Existing navigation is verified by real pointer regressions in both the Timeline and Composition viewer; each opens the nested comp's own tab. No speculative navigation change was needed. |
| #570 viewer Cmd/Ctrl +/- | Focused Composition viewers accept these aliases while preserving explicit shortcut bindings and text editing. `command_plus_minus_zoom_the_focused_composition` passes. |
| #553 motion-path vertex tools | Pen/Add/Delete/Convert tools and contextual cursors now work on motion paths, including tangent dragging and undo. Insertion subdivides spatial/temporal curves and keeps layer-time semantics. Hold, roving, reversing/overshooting and degenerate insertions return explicit unsupported errors rather than silently changing motion. |
| #659 text effect clipping | Text effects can draw beyond the glyph raster, including full/half-resolution CC Tiler and CC Smear. Controls and reviewed color-only effects retain tight rasters. CPU and Metal comparisons pass. |
| #658 CC Smear above 100 | Reach accepts -1000…1000; the default-tip brush now follows measured AE behavior and produces the extended/thinned droplet. Three CPU regressions, renderer coverage and Metal comparisons pass. See [measurements and limits](../fidelity/cc-smear.md). |
| #651 GIF playback | GIFs use their frame delays in preview/export rather than decoding only the first frame. Tests cover unequal/zero delays, looping and rate interpretation; older still-GIF metadata is repaired on footage checking. |
| #612 Gradient Edit | Timeline and Effect Controls open a shared color/opacity-stop editor with midpoints, Apply, Cancel and undo. Full-app pointer tests verify a real Gradient Fill, including Escape discarding a draft. A local screenshot was inspected. |
| #649 drop into an empty timeline | Dropped footage creates a composition using its dimensions, frame rate, duration and pixel aspect. Video/still pointer tests verify one-step undo preserves the source footage. |

## Validation

Focused engine, host, effects, keyframe, UI and GPU regressions were run; all checked-in repro
scripts replay cleanly. The UI command/widget automation audit passes. Changed crates passed
all-target Clippy with warnings denied, and the browser target passed `cargo check`. CPU/Metal
effect comparisons and the six Windows/DX12 checks are separate evidence, not a claim that every
workflow was tested on every platform. Full workspace CI and an ordinary app/release rebuild were
not run locally; the later cloud CI result is recorded below.

Private logs, the gradient screenshot and raw AE-generated reference frames stay under ignored
`plan/user-bugs-validation/` and `plan/aftereffects/ref/user-bugs-658/`; no reference media ships
with these changes. The test logs retain the initial failures and their successful reruns.

## Follow-up verification, 11 October

Fourteen focused playback tests passed: six native audio-clock tests, six UI audio tests and two
playback-pacing tests. The deterministic clock tests cover 29.97/30 fps, 44.1/48 kHz, 2048/4096-sample
buffers, future callback timestamps, underruns and restart. They do not measure a physical audio
device's timestamp accuracy. The reporter's reference recording shows audio-enabled preview, but
does not reveal the device buffer size or establish audio timing as its sole cause.

The watchdog on `d51f0cf1` passed all-target Clippy but failed the unchanged tracking wall-clock
test amid parallel tests (153.49 ms/frame against 100 ms). Normal tests now retain its 0.25-pixel
accuracy assertion; the original 100 ms budget remains an explicit isolated benchmark. Both focused
checks passed locally (2.98 ms/frame for that benchmark), and tracking Clippy passed. See
[the benchmark command](../testing.md#single-point-tracking-budget).

Warm build reuse also exposed a contributor-credit lookup that embedded a deleted worktree's path.
It now resolves the current checkout at build-script runtime. Fresh generated credits matched all
27 contributor entries and 656 commits in the repository data. App/UI all-target Clippy passed.

Windows presentation-device recreation remains separate work; the owner explicitly deferred its
hardware verification. No additional full CI or ordinary app/release build was run locally.

The next watchdog (`f7089e29`) exposed a Puppet interaction race: a completed mesh could move a
Bend pin's scale handle between the displayed press target and drag-threshold hit-testing. The
unchanged pointer test also failed locally with scale stuck at 100 %. Completed geometry now waits
until the viewer captures the gesture; live pin values continue updating, and mesh adoption resumes
during the established drag. Property-panel pointer interactions do not hold the viewer's mesh.
Five existing Puppet pointer tests and six focused worker/input tests pass, including deterministic
completion injection before capture and verification that feedback resumes before release.

The replacement watchdog on `5e86b07f` exposed a second, distinct problem: post-frame tests and
between-frame control requests preferred the registry's older geometry. The Puppet test could then
press a stale handle coordinate and report 208 % instead of 200 %. A deterministic two-pixel stale
snapshot reproduced the error locally at 181.8 % before correction. These callers now use only the
latest painted elements; the existing during-paint lookup contract remains unchanged. Removed
widgets cannot be clicked through an older snapshot. All 48 viewer tests, six control-channel tests,
seven time-control tests and the new control geometry regression pass (62 total), with the original
scale tolerance unchanged.

## Full CI acceptance

The [watchdog on `3f5c7d85`](https://github.com/eliasx45/effectcraft/actions/runs/38102786467)
completed successfully on 11 October 2026. The lockfile change selected full workspace CI:
3,168 tests passed, zero failed and 44 were ignored. Formatting (867 files), all-target workspace
Clippy with warnings denied, dependency layering, attribution (40 assets), and WebAssembly checks
(34 crates) passed. Both Puppet scale regressions and the current-frame control geometry test
explicitly passed in this run. The full CI step took 11 minutes 57 seconds.

Native Windows presentation recovery remains a separate, unmerged candidate with hardware
verification deferred by the owner. Report #580 still needs a reproduction or crash diagnostic;
#660 remains unresolved without an EffectCraft-specific reproduction, as requested.
