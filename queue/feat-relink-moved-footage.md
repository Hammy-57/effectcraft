title: feat(footage): relink footage that moved with its folder or project
branch: upstream/feat-relink-moved-footage
depends: none

## What
Relinking footage that moved, as in After Effects:
- `footage.relink {item, path}` points a missing item at its new file, keeping its interpretation. It also relinks every other missing item that was in the same place relative to it (the folder moved, or a drive was renamed). All of this is one undo step.
- File ▸ Replace Footage on a missing item does the same.
- `footage.missing` lists the missing items.
- The footage check after File ▸ Open finds files that moved along with the project's folder, and doesn't mark the project modified.
- Paths are compared by their parts, so projects made on Windows relink on macOS and the other way round.

## Why
A moved project folder or a renamed drive used to leave every footage item missing, one to fix at a time (gaps.md G4).

## How tested
- Engine unit tests: `relink` / `footage_check` / `replay` (14 passed). Host test: `relink` (1 passed).
- `cargo clippy -p effectcraft-engine -p effectcraft-host --all-targets -- -D warnings` and rustfmt pass on this branch.
- Upstream port: upstream's folder scan (`file.relinkFootage`, relink.rs) is unchanged. This adds `relink_moved.rs` next to it, and leaves out the fork's own folder search.
