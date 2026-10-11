title: fix(cli): render --comp accepts a comp id like render-frame
branch: upstream/cli-render-comp-id
depends: none

## What
`effectcraft-cli render --comp <x>` now resolves the comp the same way `render-frame` does: a number is a comp id, `-` is the active comp, anything else is a name.

## Why
`render --comp 2` was sent as the name "2", so a comp could not be picked by id from the command line, unlike `render-frame --comp 2`.

## How tested
- New CLI test in `apps/effectcraft-cli/tests/cli.rs` renders by comp id.
- `rustfmt --edition 2024 --check` and `cargo check -p effectcraft-cli` on this branch.
