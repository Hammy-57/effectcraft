title: feat(import): File ▸ Import ▸ EffectCraft Project merges a project into the open one
branch: upstream/feat-import-effectcraft-project
depends: none

## What
File ▸ Import ▸ EffectCraft Project… (`file.importProject {path}`; `.ecproj` / `.ecprojx` given to File ▸ Import also work) merges a project into the open one:
- The imported project goes in a folder named after the file.
- Every id moves past the open project's ids.
- It's one undo step.
- Relative footage resolves next to the imported file.
- The result lists the comps, the root comps, `primaryComp` and missing media.

`offset_ids` also shifts tracker Motion Targets now. It and `fix_next_id` saturate instead of overflowing, so a damaged file can't overflow on open or import.

## Why
After Effects can import one project into another. This was missing.

## How tested
- `tests_import_project` (engine).
- `cargo clippy -p effectcraft-engine -p effectcraft-project -p effectcraft-ui-egui --all-targets -- -D warnings` and rustfmt pass on this branch.
