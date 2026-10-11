title: fix(import): fuzzing finds: self-listing PDF page tree hang, huge path coordinates
branch: upstream/fix-fuzz-crashes
depends: none

## What
Two crash/hang fixes found by fuzzing untrusted input:
- **PDF:** a page tree whose `Pages` node lists itself in `Kids` (several times) was walked once per path, 3^32 visits, and hung the import. The walk now has a visit budget.
- **Paths:** shapes with huge coordinates (e.g. 1e30) are clipped to a safe range before tiny-skia rasterises them, instead of overflowing/hanging the rasteriser.

## Why
"Never crash": imported files (PDF/AI/EPS, SVG, Lottie, AEP shapes) are input we don't control. Both cases were found by cargo-fuzz targets.

## How tested
- Regression tests: `crates/pdf/src/tests.rs` `a_page_tree_that_lists_itself_doesnt_hang`; new tests in `crates/path` for huge coordinates.
- On this branch (upstream/main + these commits): `cargo clippy -p effectcraft-pdf -p effectcraft-path --all-targets -- -D warnings`, `cargo test -p effectcraft-pdf -p effectcraft-path` (all pass), rustfmt --check.
