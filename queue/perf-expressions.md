title: perf(expr): prefetch host reads and share expression results between evaluations
branch: upstream/perf-expressions
depends: none

## What
Expressions run fewer times per frame, and each run is cheaper:
- A run collects every host value it is missing at once, and prefetches the values the previous evaluation read. Keyframe lists are read lazily. `sourceRectAtTime` on shape layers is computed geometrically, and a layer's own path is memoised.
- Expression results are shared between evaluations through a per-thread memo keyed by script, comp, layer, property, comp time and the `__begin` inputs. On a hit, the memo answers the recorded host requests at the evaluation's own depth. It returns the stored result only when every answer is bit-identical, so an edit can never serve a stale result. Scripts that use `Math.random` or `Date` are never stored. The memo is bounded by answer bytes.

## Why
A slider read by N layers now runs once per frame, not N times. On a 100-layer expression rig a frame drops from 23.4 to 14.7 ms.

## How tested
- `tests_perf.rs`: representative expressions give unchanged results, chained lookups and loops over layers run a bounded number of times, stale prefetch predictions and stand-in answers never reach a result, and shape `sourceRectAtTime` matches the rendered buffer.
- `tests_memo.rs`: results match fresh runs on a thread with the memo off, and the byte bound holds.
- On this branch on top of `main`: `cargo clippy -p effectcraft-expr -p effectcraft-render --all-targets -- -D warnings` and `cargo test -p effectcraft-expr -p effectcraft-render` pass (render goldens included).
