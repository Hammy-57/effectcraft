title: feat(xtask): ci --affected, a fast CI profile and replayable crash repros
branch: upstream/ci-affected
depends: none

## What
- `cargo xtask ci --affected`: runs fmt, clippy, tests, layers and assets only for the crates a change can affect (the changed crates and their dependents), documented in `docs/testing.md`.
- A `ci` cargo profile (no thin LTO, 16 codegen units) for `cargo xtask ci`, and looser timing tests that failed under load.
- `cargo test` / nextest in `xtask ci` and `ci --affected` run with `--no-fail-fast`, so one run lists every failing crate.
- Replayable crash repros: `journal.export` / `help.exportRepro` save the session's command journal as JSON lines, the last-resort guard writes one automatically on a caught panic, `effectcraft-cli replay` runs it headless, and `tests/repros/*.jsonl` are replayed by `crates/host/tests/repros.rs` (three repros included).

## Why
The full gate spent about 1.5 h in LTO linking ~69 test binaries while the tests themselves took about 75 s, so contributors skipped it. A crash report should become a regression test without hand-writing code.

## How tested
- `cargo test -p xtask` (18 passed)
- `cargo test -p effectcraft-engine --lib -- replay journal` (6 passed)
- `cargo test -p effectcraft-host --test repros` (1 passed, replays the included repros)
- `cargo check -p effectcraft-cli -p effectcraft-engine -p effectcraft-host -p xtask --tests`
Not run: `ci --affected` end to end, the CLI `replay` integration test, and no full `cargo xtask ci`.

Independent of the other PRs split from #859 (it touches `crates/engine/src/lib.rs` and the CLI usage text next to them, so a trivial rebase may be needed).

Split out of #859 at the maintainer's request on #858.
