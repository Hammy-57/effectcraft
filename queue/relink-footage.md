title: feat(footage): relink footage that moved
branch: upstream/relink-footage
depends: none

## What
Relink footage that moved, as in After Effects:

- `footage.relink {item, path}` relinks one item and every other missing item that moved with it
  (same relative layout), as one undo step; Replace Footage on a missing item does the same.
- `footage.missing` lists missing items.
- The footage check after open finds files that moved along with the project's folder, without
  marking the project modified.
- Paths compare by parts, so Windows and macOS projects relink either way.

The folder search stays with upstream's `file.relinkFootage`; this lives in `relink_moved.rs` next
to `relink.rs`.

## Why
Moving a project folder (or a footage folder) left every item missing, to be relinked one by one.
Split out of #858 as asked.

## How tested
- `crates/host/tests/relink.rs` and the engine `relink` / `footage_check` tests; written with the
  change, not re-run on this branch.
- `rustfmt --check` and `cargo check` on the affected crates on current upstream `main`.
