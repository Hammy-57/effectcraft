title: feat(ui): Effect Console (Ctrl+Space) to search and apply effects and presets at the pointer
branch: upstream/effect-console
depends: none

> **Overlaps #918** (Effect Console by @vadmonboss). We're happy to defer to that PR: if you'd
> rather take #918, close this one; anything useful here (the ranking, presets, automation ids,
> tests) is free to borrow.

## What
Edit ▸ Effect Console... (Ctrl+Space, `app.fxConsole {query?}`): a small search bar at the pointer
that applies effects and presets to the selected layers.

- Ranks effects, text presets and user presets by name, word starts, initials (`gb` = Gaussian
  Blur) and letters in order; recent and favourite effects rank higher, Obsolete ones lower.
- Enter applies and closes, Shift+Enter applies and stays open, arrows choose, Escape / Ctrl+Space
  / a click outside close. With no text it lists recent effects.
- Off macOS a Ctrl shortcut also expects the command flag egui reports with Ctrl (otherwise
  Ctrl+Space never matched there).
- `docs/control-protocol.md` lists `app.fxConsole` and its automation ids; the menu entry is
  translated in all ten catalogues.

## Why
A fast, keyboard-first way to apply effects at the pointer (like the popular Effects Console
extension for After Effects). Split out of #858 as asked.

## How tested
- `crates/ui-egui/tests/ui_fx_console.rs` (open at the pointer, ranking, apply, Shift+Enter,
  close); written with the change, not re-run on this branch.
- `rustfmt --check` and `cargo check` on the affected crates on current upstream `main`.
