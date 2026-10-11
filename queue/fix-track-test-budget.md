title: test(track): run the 1080p tracking time budget apart from the parallel suite
branch: upstream/fix-track-test-budget
depends: none

## What
The 1080p single-point tracking test keeps its accuracy check (error < 0.25 px) in the normal suite; its 100 ms/frame wall-clock budget moves to a separate `#[ignore]` performance test, documented in docs/testing.md with the command to run it alone.

## Why
Under a parallel test suite (or a busy machine) competing tests inflate elapsed time, so the budget check failed without any tracking regression.

## How tested
- `cargo test -p effectcraft-track` (56 passed, 4 ignored) and `cargo clippy -p effectcraft-track --all-targets -- -D warnings` on this branch; rustfmt --check.
- The isolated benchmark measured 2.98 ms/frame on the original machine.
