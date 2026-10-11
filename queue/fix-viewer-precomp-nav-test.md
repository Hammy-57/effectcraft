title: test(viewer): double-clicking a precomp in the viewer opens its own tab (#637)
branch: upstream/fix-viewer-precomp-nav-test
depends: none

## What
Regression test for #637: double-clicking a precomp layer in the Composition viewer opens the precomp in its own Timeline tab and makes it the active comp (the parent comp's tab stays open).

## Why
#637 reported nested composition navigation from the viewer; the existing test covered only the Timeline double-click path. This pins the viewer path.

## How tested
- `cargo test -p effectcraft-ui-egui --test ui_tabs` on this branch (11 passed, 1 ignored); clippy `--test ui_tabs -D warnings`; rustfmt --check.
