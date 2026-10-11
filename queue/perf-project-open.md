title: perf(project): stream property nodes on load; skip copying current effect instances
branch: upstream/perf-project-open
depends: none

## What
- Property-tree nodes stream during project loading. `Node` was an internally tagged serde enum, so serde buffered every node subtree into its private `Content` tree before converting it. A hand-written `Deserialize` now reads the `"node"` tag when it comes first, which serialization always does, and streams the rest of the map into `Property`/`PropGroup`. A tag found later falls back to buffering. Serialization is unchanged (still derived).
- Opening a project no longer deep-clones every effect instance to compare it with its upgraded form. `migrate::is_current` is a read-only check that mirrors `upgrade_instance`, and current instances return early.

## Why
In a release build, parsing a 20.9 MB synthetic layer array (200 layers × 5 animated properties × 200 keys) drops from 226 ms to 72 ms.

## How tested
- `is_current` also sees the Hidden/Text → File parameter upgrade (#514), so old Apply Color LUT and OCIO instances still upgrade (last commit).
- A test checks that `is_current` never skips a needed upgrade, over per-parameter variants of every registered effect. The project round-trip and loading tests still pass.
- On this branch on top of `main`: `cargo clippy -p effectcraft-project -p effectcraft-effects --all-targets -- -D warnings` and `cargo test -p effectcraft-project -p effectcraft-effects` pass.
