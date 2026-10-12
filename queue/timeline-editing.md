title: feat(timeline): AE trim zones, click-to-deselect, Type tool on overlapping text, ruler labels
branch: upstream/timeline-editing
depends: none

## What
Timeline editing closer to After Effects, plus small panel polish:

- **Trim zones** straddle the layer bar ends (4 pt outside, up to 5 pt inside; short bars keep
  their middle third) and win over the slip zone, with the resize cursor.
- **Click on empty track area** deselects layers.
- **Type tool on overlapping text**: the Type tool edits the selected text layer when text layers
  overlap, respecting visibility (with tests that reproduce the duplicated-text case).
- **Ruler labels** that would run under the marker bin are left out instead of clipped to a stray
  digit.
- Polish: long Project panel names end in "…"; Effect Controls' Reset / About... sit on the header
  row's centre.

Left out of #858's version: "trimming a selected layer trims every selected layer", since #950
(merged) already does that; and "centre text in search fields", already on `main` (#581).

## Why
After Effects parity for everyday timeline editing. Split out of #858 as asked.

## How tested
- `ui-egui/tests/ui_timeline_trim.rs`, `ui_workflow_selection.rs`, `ui_text_edit.rs`; written with
  the changes, not re-run on this branch.
- `rustfmt --check` and `cargo check --all-targets` on the affected crates on current upstream
  `main`.
