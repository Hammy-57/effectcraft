title: feat(xtask): cargo xtask bench render benchmark with --fail-over regression gate
branch: upstream/feat-xtask-bench
depends: none

## What
`cargo xtask bench`: a render benchmark of generated scenarios (a demo comp; 1080p with 100 layers and common effects; precomps nested three deep; PNG-sequence, H.264 and 4K footage). It renders each at Full and Half, records peak memory, CPU and wall time per run in `target/bench/results.jsonl`, and compares against an earlier run with `--compare`. `--fail-over PCT` exits non-zero when a resource gets more than PCT % worse, so it can gate a change.

## Why
gaps.md asks for benchmark numbers tracked over time, with regressions failing a check. This gives every optimisation a before and an after number.

## How tested
- `cargo clippy -p xtask --all-targets -- -D warnings` and rustfmt on this branch (upstream/main base).
- Upstream port: without `--bin`, the CLI is built with `--release` into `target/bench`. The fork used its own build profile there.
- Not run here: a full benchmark (it takes minutes and needs a quiet machine).
