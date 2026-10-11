title: fix(preview): Cache Frames When Idle follows the Preview panel range
branch: upstream/idle-cache-preview-range
depends: none

## What
- RT8: Cache Frames When Idle follows the Preview panel range (Play Around Current Time caches the pre-roll)

## Why
See the commit messages; each commit is cherry-picked from eliasx45/effectcraft (`-x` trailer).

## How tested
TODO before opening: confirm what ran. The commits' own test notes:
- (none in the commit messages)
- tests added or changed: crates/ui-egui/tests/ui_preview_align.rs 
- queue-pr.sh: rustfmt --check and cargo check on the affected crates passed on this branch
