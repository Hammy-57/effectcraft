## Summary

| Measure | Value |
|---|---|
| **Feature parity, weighted by tier** (P0 ×3, P1 ×2, P2 ×1) | **≈ 64%** |
| Feature parity, unweighted | ≈ 58% |
| P0 (it isn't After Effects without it) | ≈ 75% |
| P1 (professional daily use) | ≈ 38% |
| P2 (long tail) | ≈ 17% |
| Features done / partial / missing | 18 / 60 / 14 of 92 |
| **Effects** | **298 of 298** After Effects 2026 effects by name (100%, after M9.11, Warp Stabilizer and the 3D Camera Tracker; was 257) |
| Remaining work | ≈ **167 agent-hours** (139 h of features + ≈ 20% for 1:1 polish against After Effects) |
| Wall-clock estimate | ≈ **42–50 hours** with five agents working in parallel and one integrating; ≈ 31–35 h for P0 + P1 only |

"Agent-hours" are hours of one autonomous coding agent (Claude Opus 5.5) working in its own git
worktree with tests. The estimate is calibrated on this repository's own history: Classic 3D
(cameras, lights, shadows, depth of field, views, tools) took one agent about 2.7 h; timeline
depth (graph editor, keyframe dialogs, time remapping, pen and masks) about 2.3 h; 85 effects
about 1.2 h; the After Effects menu bar about 1.3 h; layer styles about 0.6 h; the web build
about 0.8 h. What is left is harder on average (tracking, roto, puppet, GPU, Advanced 3D), and
sequential chains (tracker → mask tracking → Warp Stabilizer → 3D camera tracker; GPU
compositor → GPU effects) limit how much can run in parallel.

Work in progress at the time of the audit, not yet counted: motion tracking, preferences and the
keyboard shortcut editor, auto-save and crash recovery, paint and puppet tools, Lottie import and
export.

## Update after the second wave (same day, commit `e1f7241`)

Motion tracking (1-, 2- and 4-point, stabilize, corner pin), paint (Brush, Clone Stamp, Eraser
with write-on) and the puppet tools, preferences, a keyboard shortcut editor that rebinds,
auto-save and crash recovery, Lottie import and export, Effect Controls widgets (angle dial,
point crosshair, eyedropper, curves and levels editors, effect copy/paste) and 20 more menu
commands landed. Re-scoring only the features they touch:

| Measure | Before | After |
|---|---|---|
| Feature parity, weighted | 63.7% | **≈ 70%** |
| P0 / P1 / P2 | 75% / 38% / 17% | 78% / 56% / 17% |
| Remaining, audit estimate | 167 agent-hours | ≈ 141 agent-hours |

**Calibration.** The audit priced this wave at about 21 agent-hours; five agents delivered it in
about 1.5 hours of wall-clock time (roughly 6 agent-hours of actual work), though some features
landed at 80–90% rather than fully done. So the per-feature estimates above are conservative.
Remaining wall-clock time with five parallel agents is therefore somewhere between **≈ 12–15
hours** at the pace observed so far and **≈ 35–40 hours** if the audit's figures hold. The
hardest remaining systems (Roto Brush, Warp Stabilizer, the 3D camera tracker, Advanced 3D, GPU
rendering, on-canvas text editing) are where the conservative figure is most likely to be right.

## Update at the end of 2 October 2026

A third wave landed: viewer interactions (snapping, rulers, channels, exposure, snapshots, region
of interest, shape pen and pen-tool family, motion-path handles, graph editor transform box),
on-canvas text editing with per-character styles and paragraph settings, render fidelity (8/16/32
bpc, Rec.709/Rec.2020/P3 working spaces and linear blending, frame blending with optical flow,
collapse transformations, slip edits), a GPU compositor with 16 GPU effects, Wiggler/Smoother/
Motion Sketch, the marker dialog, precompose options and the comp navigator, Project panel folder
moves/rename/columns/thumbnails, drag-and-drop docking with floating panels, and the tracking
library groundwork for mask tracking and Warp Stabilizer.

| Measure | Morning | Now |
|---|---|---|
| Feature parity, weighted | 63.7% | **≈ 79%** |
| Unweighted | 57.8% | ≈ 74% |
| P0 / P1 / P2 | 75% / 38% / 17% | 87% / 67% / 21% |
| Remaining (audit-scale agent-hours) | ≈ 167 | ≈ 100 |
| Wall clock, five parallel agents | 42–50 h | **≈ 8–10 h at today's measured pace; ≈ 25 h by the audit's conservative figures** |

What is left is concentrated in large systems: the Warp Stabilizer and mask-tracking UI on top of
the new tracking library, and wasm threads. (The missing effect categories, 3D Channel, Immersive
Video and OCIO, landed in M9.11; the scripting object model in M14.4; Roto Brush and Refine Edge in
M6.7; native macOS menus, the Composition Flowchart, Timeline column/search depth, the Home screen
and every After Effects workspace in the UI-polish wave; PSD/SVG import, WebM/WAV/AIFF output and
the disk cache in the formats wave; the 3D Camera Tracker in M12.6.)
