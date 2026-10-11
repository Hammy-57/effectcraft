title: fix(text): the Type tool edits the selected layer among overlapping text layers
branch: upstream/feat-type-tool-overlapping-text
depends: none

## What
The Type tool edits the selected layer when text layers overlap, for example a duplicated text box over its original. Hidden layers are skipped.

## Why
After duplicating a text layer (Ctrl+D) and moving it, clicking the original with the Type tool edited the wrong layer.

## How tested
- `ui_text_edit`: two new pointer tests reproduce the duplicated-and-overlapping case.
- clippy `-D warnings` and rustfmt pass on this branch.
