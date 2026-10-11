# Parity with After Effects

Cells hold the current state only; append history to [parity-log.md](parity-log.md).

How close EffectCraft is to After Effects 2026, feature by feature, and how much work is left.

> **Read this as a measure of breadth, not of real-world readiness.** It checks whether each
> feature exists, using our own catalogue, graded by the agents that built it. It does not measure
> whether the feature behaves like After Effects (nothing compares the two yet), whether After
> Effects projects open (only through the py-aep converter), or how the app holds up for real
> users on every platform.
> Those gaps, and the work that closes them, are in [gaps.md](gaps.md). Don't raise the numbers
> here without the evidence gaps.md asks for.

## Current status (audit at commit `d39c0e8`, 4 October 2026; updated 5 October for M3.9–M4.12)

| Measure | Value |
|---|---|
| **Feature parity, weighted by tier** (P0 ×3, P1 ×2, P2 ×1) | **≈ 99%** counting every partial feature as half done; ≈ 99.8% using per-feature fractions |
| Unweighted | ≈ 98.4% (half-credit) / 99.6% (fractions) |
| P0 / P1 / P2 | 100% / 98% / 90% (half-credit); 100% / 99.7% / 97.5% (fractions) |
| Features done / partial / missing | 89 / 3 / 0 of 92 |
| **Effects** | **306** effects, every one implemented in full ([effects.md](effects.md)); 280 run on the GPU |
| Disabled menu entries left | 1, on purpose: Import ▸ Vanishing Point (.vpe), whose format has no public specification |
| Remaining work | ≈ 8.5 agent-hours at the pace measured so far (≈ 26 on the conservative audit scale); ≈ 3.5 without the learned-model items |
| **Wall-clock estimate** | **≈ 2–3 hours** with five agents in parallel; ≈ 1 hour without the learned-model items |

Partial features:

| id | Tier | Done | What is missing |
|---|---|---|---|
| EFF-5 GPU effects | P1 | 0.98 | Fractal (its escape iteration needs f64) and the analysis / tool effects (Warp Stabilizer, Roto Brush, Camera Tracker, Paint…) render on the CPU; the 13 audio filters stay on the CPU by design |
| MSK-4 Roto Brush | P2 | 0.85 | Roto Brush 2.0 / 3.0 can use a trained model since M13.35 (MobileSAM, optional download); its quality has not been compared with After Effects (G1), so the score stands |
| TRK-3 Face tracking | P2 | 0.9 | Can use a trained model since M13.36 (MediaPipe Face Landmarker, optional download); the classical fitter is weak on profile and occluded faces; neither is compared with After Effects (G1), so the score stands; Rolling Shutter Ripple is approximated |

Since this audit: the project operations (M3.9–M3.14) and preview (M4.5–M4.12) passes fixed
behaviour inside features already counted as done, so the counts above stand: unsaved-changes
prompts and a modified mark that follows undo, Solid / Layer Settings with "Affect all layers that
use this solid", lock-aware layer commands, New Comp from Selection's options, menu-bar fixes,
project files that always reopen; RAM preview keys and eviction, Cache Before Playback that fits,
Cache Frames When Idle, Preserve frame rate / resolution when nested, nested comps past their end
and one motion blur gate (see [parity-log.md](parity-log.md)). Their code was checked against
[AGENTS.md](../AGENTS.md)'s "Never crash" rules (M4.12).

Before this audit: M13.22 and M13.23 put 70 more effects on the GPU (236) and composite every
Advanced 3D mode on the GPU; M13.24 runs GPU effects in the browser's render workers with a disk
cache in browser storage and a storage manager; M13.25 added the Home template gallery, Save as
Template and View ▸ Simulate Output ▸ My Custom RGB; M13.26 Premiere Pro interop through FCP7 XML,
FCPXML, OTIO, EDL, AAF and OMF (import and export, with pre-renders for layers a timeline can't
hold); WebM VP9 alpha now imports (FilmCraft); M13.27 pins the CPU simulation effects with golden
hashes.

Out of clean-room scope (no public specification or no permissively licensed data): native
`.prproj`, `.jsxbin`, Vanishing Point `.vpe`, the predefined CJK CMaps in PDF import, Kodak film
emulations. Adobe service integrations (Team Projects, Libraries, Media Encoder, Dynamic Link,
Frame.io, Exchange) and third-party plug-ins (Cinema 4D, Mocha) are intentionally absent.

Earlier audits and the per-milestone update sections are in [parity-log.md](parity-log.md).

## By area

Every catalogued feature in every area exists (89 done, 3 partial, above). This table used to
give a per-area percentage from an older audit; those numbers had gone stale (Paint read 0% after
the paint tools landed, Animation 70% after Wiggler, Smoother and Motion Sketch did) and were
removed on 10 October 2026 rather than guessed again. What it holds now is what is known to be
missing in each area, and whether anything in it has been measured against After Effects (G1 in
[gaps.md](gaps.md)): "unmeasured" means nobody has compared it yet, not that it differs.

| Area | Known gaps | Measured against After Effects |
|---|---|---|
| Layers | none catalogued | unmeasured |
| Output | HEVC/AV1 export lacks B-frames, multi-reference, SAO/CDEF/restoration and alpha; Opus lacks FEC/DTX (20 ms frames only); no Photoshop sequence output; movie overflow checked only at file creation | unmeasured |
| Audio | none catalogued | unmeasured |
| Import | JPX / JBIG2 images and predefined non-Identity CJK CMaps inside PDF/AI files, Illustrator EPS relying on Adobe procsets, PSD 3D layers; `.aep` only through the py-aep converter, `.aepx` not read | 255 SceneYard templates through py-aep: median mean-absolute error 0.018 against After Effects' renders, 2 clearly off ([gaps.md](gaps.md) G4) |
| Automation | `.jsxbin`; ExtendScript `#include` / `#target`; Startup and Shutdown script folders | unmeasured |
| Shapes | Lottie can't carry stroke taper/wave | unmeasured |
| Compositions | Mocha-style planar tracks for templates | unmeasured |
| Animation | none catalogued | 8 scalar ease cases (168 samples) match AE 26.3 to 3e-11 degrees ([fidelity/README.md](fidelity/README.md)); spatial paths, auto Bezier, roving, time remapping and expressions unmeasured |
| Text | no extruded strokes | unmeasured |
| Web | no shared-memory threads inside one engine instance (one instance per worker by design; a threaded build needs nightly `build-std`) | unmeasured |
| 3D | reflections of other layers (Advanced 3D reflects only the Environment light, #261), multi-view layouts, the Extended Viewer for Advanced 3D comps | unmeasured |
| Effects | Fractal and the analysis / tool effects render on the CPU (280 of 306 on the GPU); the controls listed as partial in [effects.md](effects.md) | unmeasured (CPU and GPU agree with each other, which is not the same thing) |
| Interface | multi-monitor support (#227); accessibility; Japanese panel strings | unmeasured |
| Project | OCIO displays beyond the built-in tone map | unmeasured |
| Masks & roto | Roto Brush's model not compared with After Effects | unmeasured |
| Preview | GPU versions of the audio-driven effects and the stepped particle systems | unmeasured |
| Tracking | face tracking's trained model is an optional download; without it the classical fitter is weak on profile and occluded faces; Rolling Shutter Ripple is approximated | unmeasured |
| Paint | none catalogued | unmeasured |

## Effects still missing

The full catalogue with GPU / 32-bpc badges and per-effect status is [effects.md](effects.md)
(generated from the registry).

None: the 3D Camera Tracker landed in M12.6. Boris FX
Mocha and Cineware are third-party and not counted.

### Added in M9.11 (39 effects)

| Category | Effects | Notes |
|---|---|---|
| 3D Channel | 3D Channel Extract, Cryptomatte, Depth Matte, Depth of Field, EXtractoR, Fog 3D, ID Matte, IDentifier | Read auxiliary channels: multi-layer OpenEXR (any `layer.channel`, Cryptomatte manifests from the header; EXtractoR picks a layer and channels from popups of the footage's layers and channels, `layer.channels`, #295; the footage itself shows the beauty pass, never Cryptomatte or depth, #412) or the compositor's own depth / layer-ID / normal / UV / Cryptomatte pass for precomps of 3D comps. |
| Immersive Video | VR Blur, VR Chromatic Aberrations, VR Color Gradients, VR Converter, VR De-Noise, VR Digital Glitch, VR Fractal Noise, VR Glow, VR Plane to Sphere, VR Rotate Sphere, VR Sharpen, VR Sphere to Plane | Seam-aware equirectangular processing (wrap-around, pole continuation, latitude-scaled filters), mono / over-under / side-by-side layouts; VR Converter between equirectangular, cube maps (4:3, 3:2, 6:1), sphere map and 2D. |
| Color Correction | OCIO CDL / Color Space / Display / File / Look Transform, Color Stabilizer | Built-in minimal OCIO-style config (ACES2065-1, ACEScg, ACEScct, sRGB, Rec.709, Rec.2020, Display P3, linear variants, XYZ) from published matrices and transfer functions; `.cube` / `.3dl` / `.csp` / `.spi1d` / `.spi3d` / `.spimtx` LUTs and ASC `.cc` / `.ccc` / `.cdl`. Custom `.ocio` config files are read from the OCIO v2 documentation (Configuration ▸ Custom: colorspaces and display colorspaces with the scene / display reference spaces joined by the default view transform, roles, displays with v1 and v2 views, view transforms, looks; Matrix, File, Exponent, ExponentWithLinear, Log, LogAffine, CDL, Range, Allocation, ColorSpace, Look, DisplayView, Builtin (matrices and display curves) and Group transforms), so Blender 4.x / 5.x configs give the Standard, AgX, Filmic and False Color views (#410). Grading Primary / Tone transforms, the ACES output and gamut-compression builtins and HLG pass colours through, and the effect shows a warning naming them (`effect.warning`). |
| Distort | CC Flo Motion, Liquify, Rolling Shutter Repair | Liquify strokes are effect data replayed into a displacement mesh (`liquify.stroke` / `liquify.clear` commands; no interactive viewer brush yet). Rolling Shutter Repair uses the KLT tracker. |
| Blur & Sharpen | Camera-Shake Deblur, CC Radial Blur | Deblur substitutes aligned patches from sharper neighbouring frames. |
| Audio | Compressor, Distortion, Gate | Applied in the mixdown like the other audio effects. |
| Simulation | CC Hair, Particle Playground | Particle Playground: cannon, grid, layer exploder, layer map, gravity, repel, wall, persistent property mapper (no Particle Exploder, text particles or ephemeral mapper yet). |
| Keying | Key Light | The full Keylight 1.2 control set under a generic name ("Keylight" is a vendor trademark); `lookup("Keylight (1.2)")` and the Effects & Presets search ("keylight") find it. Keyers' colour eyedroppers sample the effect's input (`effect.pickColor`), so a screen that is already keyed can still be picked. |
| Utility | Color Profile Converter | Our colour spaces and ACES; rendering intents (perceptual gamut compression, relative / absolute colorimetric, saturation). |
| Matte | Mocha shape | Mocha's export format is not public: reads a documented JSON shape format instead. |

## Highest-value gaps, in order

1. ~~On-canvas text editing and per-character styles~~ (landed: M9.9–M9.10).
2. ~~Effect Controls widgets: angle dial, point crosshair, eyedropper, curves and levels editors.~~ (second wave, 2 October)
3. ~~Viewer basics: snapping, rulers, channel view, snapshots, exposure, a drawable region of interest.~~ (M0.13)
4. ~~A GPU (wgpu) compositor, then GPU effects~~ (M12.2: 2D compositing and 16 GPU effects; M12.7:
   Classic 3D runs, adjustment layers, 58 GPU effects and GPU particles; M13.9: 152 GPU effects).
5. ~~Pen tool for shape paths, shape vertex editing, free transform~~ (M6.5); the Layer viewer.
6. ~~Motion-path handles in the viewer; graph editor transform box and snapping.~~ (M5.8)
7. ~~Point tracking and stabilization~~ (M6.x; Warp Stabilizer M12.5, 3D Camera Tracker M12.6).
8. ~~Frame blending, collapse transformations, slip edit.~~ (M4.3–M4.4)
9. ~~A real 8/16/32-bit pipeline with linear blending and colour management.~~ (third wave; OCIO / ACES in M7.7)
10. ~~Drag-to-dock and floating panels, saved workspaces, native macOS menus.~~ (Undocked panel groups are OS windows of their own on the desktop.)
11. ~~Auto-save, crash recovery, recent projects~~ (M3.7, M14); unsaved-changes prompts (M3.9).
12. ~~Expression gaps: `sampleImage`, `footage()`~~ (landed with data footage, the error bar and the Expression Language menu; the `sourceText` style API landed in M9.9).
13. ~~Lottie, WebM, SVG and PSD import~~ (landed; a disk cache too).
14. ~~Puppet and paint tools~~ (second wave; puppet rigging and follow-through in M13.14–M13.16).
15. ~~Preferences and a shortcut editor that can rebind; real Wiggler, Smoother and Motion Sketch; the marker dialog.~~ (second and third waves)
16. ~~Motion blur of collapsed precomps and of animated content inside a layer (shape paths, text
    animators).~~ (M4.13)
17. ~~Content-keyed RAM preview frames, so an edit keeps the frames of comps it doesn't touch.~~
    (M4.14)
18. ~~View ▸ New Viewer (several Composition viewers)~~ (M13.32); ~~nested comp markers on the
    precomp layer bar~~ (M13.33), ~~audio scrubbing~~ (M13.34).

At the original audit the engine underneath (keyframes, expressions, shape operators, text
animators, Classic 3D, all 38 blend modes, the render queue) was already deep, and most of what was
missing was interactive tooling, settings stored but not rendered, about 69 disabled menu entries
and the large systems (tracking, puppet, paint, roto). All of those have landed since; one menu
entry stays disabled on purpose (Import ▸ Vanishing Point), and what is left is listed at the
top of this page and in items 16–18 above.
