title: perf(text): cache finished text layouts, share glyph outlines and font bytes
branch: upstream/perf-text-cache
depends: none

## What
- Glyph outlines are read under a `RwLock` and transformed straight from the shared `Arc`, with no clone.
- `layout_doc` caches its finished `TextLayout`, keyed by the shaped layout, the styles and the box.
- That cache and the shaping cache are byte-bounded LRUs (64 MiB and 32 MiB) instead of holding 512 entries each.
- A font file is read once for all of its faces (`.ttc` collections). The UI shares the text engine's font bytes instead of copying the fallback fonts.

## Why
Text layers re-laid out and re-transformed identical glyphs on every frame, and font collections were read once per face.

## How tested
- An oracle test over varied documents checks that the output matches the previous path exactly.
- On this branch on top of `main`: `cargo clippy -p effectcraft-text -p effectcraft-ui-egui --all-targets -- -D warnings`, `cargo test -p effectcraft-text` and `cargo test -p effectcraft-ui-egui --lib` pass. One test, `theme::script_font_tests::arabic_words_are_joined_and_read_right_to_left`, fails on `main` in this Linux environment too.

Ported from the fork. The last two commits add the `fonts.rs` imports and drop the fork-only digit-set substitution from the test oracle.
