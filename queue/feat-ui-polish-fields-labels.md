title: feat(ui): centred field text, elided Project names, aligned effect header buttons, ruler labels clear of the marker bin
branch: upstream/feat-ui-polish-fields-labels
depends: none

## What
Four small visual fixes:
- Search and text fields centre their text and placeholder vertically. The placeholder used to sit about 2 pt above the magnifier in Timeline, Project, Effects & Presets and Home.
- Long Project panel names end with an ellipsis instead of being clipped mid-glyph.
- Reset and About… are centred on the effect header row in Effect Controls.
- Timeline ruler labels that would run under the marker bin are skipped. The comp-end label used to be clipped to a stray "1".

## Why
Visual polish found while comparing panels with After Effects.

## How tested
- `cargo clippy -p effectcraft-ui-egui --all-targets -- -D warnings` and rustfmt pass on this branch.
- Not checked: UI fingerprints. They're enforced only on aarch64 macOS, so they may need re-pinning there.
