title: feat(import): import an EffectCraft project into the open project (file.importProject)
branch: upstream/import-project
depends: none

## What
File ▸ Import ▸ EffectCraft Project... (`file.importProject`; `.ecproj` / `.ecprojx` given to
File ▸ Import do the same) merges another EffectCraft project into the open one, in a folder named
after the file, with every id shifted past the open project's, as one undo step. Relative footage
resolves next to the imported file; the result lists comps, root comps, `primaryComp` and missing
media.

`offset_ids` now also shifts tracker Motion Targets and saturates instead of overflowing;
`fix_next_id` saturates too, so a damaged file can't overflow on open or import. The menu entry is
translated in all ten catalogues.

## Why
After Effects imports one project into another (File ▸ Import ▸ File... on an `.aep`); EffectCraft
could only open projects. Split out of #858 as asked.

## How tested
- `crates/engine/src/tests_import_project.rs` (ids shifted, folders, footage paths, undo, damaged
  ids); written with the change, not re-run on this branch.
- `rustfmt --check` and `cargo check` on the affected crates on current upstream `main`.
- Adapted to current `main`: the test module list in `engine/src/lib.rs`, and es / pt-BR / ru / fr /
  de / cs menu translations.
