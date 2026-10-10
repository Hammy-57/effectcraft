> **Read this as a measure of breadth, not of real-world readiness.** It checks whether each
> feature exists, using our own catalogue, graded by the agents that built it. It does not measure
> whether the feature behaves like After Effects (nothing compares the two yet), whether After
> Effects projects open (they don't), or how the app holds up for real users on every platform.
> Those gaps, and the work that closes them, are in [gaps.md](../gaps.md). Don't raise the numbers
> here without the evidence gaps.md asks for. The per-area table ([areas/](areas/)) has stale rows that
> have not been reconciled with the headline.

## Current status (audit at commit `d39c0e8`, 4 October 2026; updated 5 October for M3.9–M4.12)

| Measure | Value |
|---|---|
| **Feature parity, weighted by tier** (P0 ×3, P1 ×2, P2 ×1) | **≈ 99%** counting every partial feature as half done; ≈ 99.8% using per-feature fractions |
| Unweighted | ≈ 98.4% (half-credit) / 99.6% (fractions) |
| P0 / P1 / P2 | 100% / 98% / 90% (half-credit); 100% / 99.7% / 97.5% (fractions) |
| Features done / partial / missing | 89 / 3 / 0 of 92 |
| **Effects** | **306** effects, every one implemented in full ([effects.md](../effects.md)); 280 run on the GPU |
| Disabled menu entries left | 1, on purpose: Import ▸ Vanishing Point (.vpe), whose format has no public specification |
| Remaining work | ≈ 8.5 agent-hours at the pace measured so far (≈ 26 on the conservative audit scale); ≈ 3.5 without the learned-model items |
| **Wall-clock estimate** | **≈ 2–3 hours** with five agents in parallel; ≈ 1 hour without the learned-model items |

Partial features:

| id | Tier | Done | What is missing |
|---|---|---|---|
| EFF-5 GPU effects | P1 | 0.98 | Fractal (its escape iteration needs f64: WGSL has none and Metal's fast math defeats double-f32 emulation) and the analysis / tool effects (Camera-Shake Deblur, Detail-preserving Upscale, Puppet, Rolling Shutter Repair, Warp Stabilizer, Mocha Shape, Roto Brush, Match Grain, Paint, Camera Tracker, Face Tracker) render on the CPU. M13.28 moved the remaining pixel effects (CC blur / glass family, Color Link, Cineon, HDR, Inner/Outer Key, Basic 3D…) and the geometry and audio generators (Lightning, Advanced Lightning, Beam, Lens Flare, Radio Waves, Vegas, Stroke, Scribble, Write-on, Paint Bucket, Eyedropper Fill, CC Glue Gun, CC Threads, Audio Spectrum / Waveform, Basic / Path Text) onto the GPU; M13.29 the particle effects (CC Particle World / Systems II, Particle Playground, CC Ball Action, CC Pixel Polly, CC Scatterize) and Curl Noise, and removed the Shatter wireframe, Foam extras, Timewarp matte, strong Warp Fisheye / Twist and custom `.ocio` fallbacks. The 13 audio filters stay on the CPU by design |
| MSK-4 Roto Brush | P2 | 0.85 | Roto Brush 2.0 / 3.0 can use a trained model since M13.35 (MobileSAM, optional download); its quality has not been compared with After Effects (G1), so the score stands |
| TRK-3 Face tracking | P2 | 0.9 | Can use a trained model since M13.36 (MediaPipe Face Landmarker, optional download); the classical fitter is weak on profile and occluded faces; neither is compared with After Effects (G1), so the score stands; Rolling Shutter Ripple is approximated |

Since this audit: the project operations (M3.9–M3.14) and preview (M4.5–M4.12) passes fixed
behaviour inside features already counted as done, so the counts above stand: unsaved-changes
prompts and a modified mark that follows undo, Solid / Layer Settings with "Affect all layers that
use this solid", lock-aware layer commands, New Comp from Selection's options, menu-bar fixes,
project files that always reopen; RAM preview keys and eviction, Cache Before Playback that fits,
Cache Frames When Idle, Preserve frame rate / resolution when nested, nested comps past their end
and one motion blur gate (see the update sections below). Their code was checked against
[AGENTS.md](../../AGENTS.md)'s "Never crash" rules (M4.12).

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

### Previous audit (commit `58163a2`, 3 October 2026, evening): ≈ 98%

87 done / 5 partial / 0 missing.

### Previous audit (commit `978e8d7`, 3 October 2026): ≈ 94%

80 done / 12 partial / 0 missing.

### Previous audit (commit `fa26ad9`, 2 October 2026, late): ≈ 93%

By area: Layers 98%, Output 95%, Compositions 95%, Automation 96%, Paint 95%, Text 95%, Import 96%,
Animation 94%, Masks 94%, Preview 94%, Interface 93%, Shapes 93%, 3D 91%, Audio 90%, Project 90%,
Tracking 96%, Effects 85%, Web 65%.

What is left, in priority order: the web app's depth (threads, storage, audio, non-blocking
renders); stroke taper/wave and multi-segment dashes; variable mask feather points; camera iris/bokeh and focus-link commands; 
approximated effects (Key Cleaner) and Liquify's viewer brush; JPX / JBIG2 images in
imported PDF/AI files (mesh shadings and CCITT landed in M13.12) and Illustrator procset EPS; and codec depth (B-frames, SAO/CDEF; HEVC and AV1 export and Opus SILK/hybrid landed in M13.4). Face tracking and a true Subspace Warp
landed in M13.3. ScriptUI (dialogs that block `show()`, palettes, dockable ScriptUI panels, File ▸ Scripts install
and sample scripts), puppet pin recording and the remaining "better than After Effects" items — a
versioned effect plug-in API with sandboxed WebAssembly plug-ins ([plugins.md](../plugins.md)),
branching undo history in the History panel, and a bit-identical rendering test across runs and
thread counts — landed in M13.1 (GPU particles landed in M12.7). Render Queue field render, crop/resize, templates and logs, and VP9 inter frames landed in M10.2.

The sections below are the original audit (morning of 2 October, ≈ 64%) and its updates, kept for
history.
