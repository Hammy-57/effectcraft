title: feat(xtask): cargo xtask bench render benchmark with --compare and --fail-over
branch: upstream/render-bench
depends: none

## What
`cargo xtask bench`: a render benchmark over generated scenarios that records peak memory, CPU
time, wall time (and max RSS / instructions retired where the OS reports them) to
`results.jsonl`. `--compare` sets two runs side by side; `--fail-over PCT` exits 1 when a matched
run got more than PCT % worse in CPU time, peak memory, max RSS or instructions (wall time follows
the machine's load and is never gated). Documented in `docs/testing.md`.

## Why
Performance work needs numbers that can be compared between commits and gated in CI. Split out of
#858 as asked.

## How tested
- xtask's `fail_over` unit test; written with the change, not re-run on this branch.
- `rustfmt --check` and `cargo check` on xtask on current upstream `main`.
