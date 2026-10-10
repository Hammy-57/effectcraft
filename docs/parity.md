# Parity with After Effects

Cells hold the current state only; append history to [parity-log.md](parity-log.md).

How close EffectCraft is to After Effects 2026, feature by feature, and how much work is left.

> **Read this as a measure of breadth, not of real-world readiness.** It checks whether each
> feature exists, using our own catalogue, graded by the agents that built it. It does not measure
> whether the feature behaves like After Effects (nothing compares the two yet), whether After
> Effects projects open (only through the py-aep converter), or how the app holds up for real
> users on every platform.
> Those gaps, and the work that closes them, are in [gaps.md](gaps.md). Don't raise the numbers
> here without the evidence gaps.md asks for. The per-area table further down has stale rows that
> have not been reconciled with the headline.

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

| Area | Weighted parity | Remaining (agent-hours) | Biggest gaps |
|---|---|---|---|
| Layers | 88% | 4.8 | motion blur of collapsed precomps (their own motion and the parent camera's) and of animated layer content |
| Output | 97% | 0.8 | HEVC/AV1 export lacks B-frames, multi-reference, SAO/CDEF/restoration and alpha; Opus lacks FEC/DTX (20 ms frames only); no Photoshop sequence output; movie overflow checked only at file creation |
| Audio | 85% | 0.5 | audio to keyframes |
| Import | 97% | 0.5 | JPX / JBIG2 images and predefined non-Identity CJK CMaps inside PDF/AI files, Illustrator EPS relying on Adobe procsets, PSD 3D layers |
| Automation | ≈ 99% | 0.1 | `.jsxbin`; the object model, ScriptUI, Essential Graphics hooks, Watch Folder and the WebAssembly effect plug-in API are in |
| Shapes | ≈ 80% | 1.5 | Lottie can't carry stroke taper/wave; taper, wave, multi-segment dashes, the pen tool and vertex editing are in |
| Compositions | ≈ 81% | 3.3 | Mocha-style planar tracks for templates; nested comp markers on the precomp layer bar |
| Animation | 70% | 9.0 | puppet depth beyond pins, recording, rigging and follow-through; Wiggler/Smoother/Motion Sketch |
| Text | ≈ 96% | 0.3 | no extruded strokes; variable font axes, OpenType features, vertical text, per-character styles and on-canvas editing are in |
| Web | 98% | 0.1 | No shared-memory threads inside one engine instance (one engine instance per worker by design; a threaded build needs nightly `build-std`) |
| 3D | 88% | 5.5 | reflections of other layers (Advanced 3D reflects only the Environment light, #261), multi-view layouts, the Extended Viewer for Advanced 3D comps |
| Effects | ≈ 85% | 3.0 | GPU versions of the remaining effects (280 on the GPU; Fractal and the analysis / tool effects on the CPU) and the missing controls listed as partial in [effects.md](effects.md) |
| Interface | 75% | 5.0 | more Learn tutorials and pixel-level fidelity of dialogs |
| Project | ≈ 68% | 5.5 | OCIO displays beyond the built-in tone map; relative footage paths and relinking when a project moves |
| Masks & roto | 74% | 5.0 | (audit estimates) Roto Brush 2.0 / 3.0 with a trained model, Refine Edge, mask tracking, Mask Interpolation and variable-width feather are in |
| Preview | 77% | 2.8 | GPU effects for the audio-driven effects and the stepped particle systems; content-keyed RAM preview frames (an edit drops every comp's), audio scrubbing, more snapshot slots |
| Tracking | ≈ 96% | 0.3 | face tracking's trained model is an optional download; without it the classical fitter is weak on profile and occluded faces; Rolling Shutter Ripple is approximated |
| Paint | 0% | 4.0 | Brush, Clone Stamp, Eraser |

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
10. ~~Drag-to-dock and floating panels, saved workspaces, native macOS menus.~~
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
