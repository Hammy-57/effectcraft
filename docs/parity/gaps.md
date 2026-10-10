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
