# Windows titlebar and input offset: issue #660

Status: **unresolved; an EffectCraft-specific Windows reproduction is needed.**

[Issue #660](https://github.com/storytold/effectcraft/issues/660) reports a black strip at the top
of a Windows 10 release window and a vertical mouse-hitbox offset. The issue has no comments
at the time of this investigation.

The attached screenshot visibly identifies **VectorCraft**: its title and welcome message say
VectorCraft, and its toolbar contains vector drawing tools. Its minimize/maximize/close buttons
are integrated into the application header. The screenshot was inspected locally and is not
included here. This does not disprove the reported symptom, but it does not demonstrate that
EffectCraft has it.

## Source findings

- [`apps/effectcraft/src/main.rs`](../../apps/effectcraft/src/main.rs) constructs the native
  `ViewportBuilder` with default decorations. It does not implement a custom Windows titlebar
  or a non-client coordinate offset.
- [`crates/ui-egui/src/lib.rs`](../../crates/ui-egui/src/lib.rs) applies UI scaling through
  `egui::Context::set_zoom_factor`; `fit_window` can request maximization when the window does
  not fit the monitor. Neither subtracts a titlebar height from pointer coordinates.
- The installed egui-winit/eframe 0.36.2 dependency sources use the client dimensions and
  pixels-per-point conversion for input and notify the presentation painter on resize.
  This source inspection alone cannot establish correct behavior on the reporter's machine.
- [VectorCraft's roadmap](https://github.com/storytold/vectorcraft/blob/main/ROADMAP.md)
  describes its own integrated titlebar on Windows/Linux, consistent with the screenshot.

## Evidence required before a fix

Reproduce in an identified EffectCraft release on Windows 10, recording the graphics backend,
monitor scale, window state (restored/maximized/fullscreen), and whether resizing or changing
monitors changes the offset. Compare a native screenshot with actual pointer/hitbox positions
at several points in the window. This distinguishes a constant frame-origin offset from a
scale mismatch or stale surface size.

No coordinate correction or decoration change was made: neither is justified by the current
evidence. No Windows runtime verification, build, or tests were performed for this investigation.
