# Graph and expanded-timeline CPU measurements

Measured on the owner's M1 Max (10 cores, 64 GiB), 14 October 2026. These are synthetic
CPU preparation probes, not input-to-visible-frame latency, GPU execution, rendering fidelity,
production-build performance or an After Effects comparison. The native app was not rebuilt or
restarted for this work. Overall performance/readiness grades are unchanged.

## Method and acceptance gate

Two ignored library probes call the actual graph `curves` and timeline `build_rows`/`expand`
functions. Graphs are 1440 pixels wide with 720 samples per curve; each curve has two rotation
keys. Expression cases use `Math.sin(time * 3) * 90`. Row cases use synthetic solid layers and
repeated instances of one source composition. Timings include constructing/dropping the output;
fixture setup and unrelated edits are outside the timer.

Each build ran three fresh-process trials, with three warmups and 25 timed samples per case per
trial. Tables show the median of the three run p50/p95 values. A process watcher rejected trials
with compiler overlap. Ordinary background apps remained running. Builds used the repository's
test/dev profile (UI opt-level 1), one compile job, and nice 10; this is not the release/local
application profile. Whole-probe peak process RSS comes from Darwin `/usr/bin/time -l`.

Before candidate measurements, the gate was set to at least 20% improvement in affected p50/p95
and at least 0.5 ms absolute p95 saving in large cases. Small-case regression tolerance was the
larger of 5% or 0.1 ms. Memory increases greater than both 10% and 32 MiB would be flagged.
Correctness and bounded cache lifetime take precedence over speed.

Baseline implementation: `e73ff269`; profiling fixture correction: `d70b212c`.
Measured candidate: `e1df9421` (graph cache `7927421f`, row sharing `9cd24d21`). The subsequently
integrated viewer-drag fix does not change either measured function. Raw samples, compiler-overlap
rejections, environment receipts and binary hashes are local under
`plan/interaction-performance-2026-10-14/`.

## Results

| CPU preparation case | Baseline p50 / p95 (ms) | Candidate p50 / p95 (ms) |
|---|---:|---:|
| 1 ordinary curve, unrelated layer edit | 0.084 / 0.094 | 0.001 / 0.001 |
| 20 ordinary curves, unrelated layer edit | 1.724 / 2.164 | 0.014 / 0.020 |
| 100 ordinary curves, unrelated layer edit | 8.257 / 8.539 | 0.082 / 0.117 |
| 100 ordinary curves, warm cache | 0.114 / 0.116 | 0.082 / 0.102 |
| 1 expression curve, unrelated layer edit | 2.061 / 2.227 | 2.053 / 2.303 |
| 20 expression curves, unrelated layer edit | 284.940 / 289.812 | 285.392 / 291.454 |
| 50 source layers, 1 expanded instance | 0.010 / 0.010 | 0.010 / 0.012 |
| 500 source layers, 1 expanded instance | 0.089 / 0.092 | 0.092 / 0.131 |
| 500 source layers, 20 instances (10,020 rows) | 1.883 / 2.055 | 1.881 / 2.029 |
| 100 source layers with properties, 20 instances (14,020 rows) | 4.513 / 4.743 | 1.380 / 1.480 |

Median whole-probe peak RSS: **72.84 MiB baseline, 72.70 MiB candidate**. This does not establish
per-feature retained memory, GPU memory or real-project editor memory. Both candidates passed the
predeclared gate. The expanded-row improvement is concentrated in repeated sources with open
properties; simple layer-only rows barely change. Single-instance overhead remains below the
absolute tolerance. Closed-row preparation stays below 0.002 ms p95 in these cases.

## Retained changes and limits

- Plain graph samples retain the actual displayed layer owners and compare their `Arc` identities.
  Unrelated edits preserve those owners; edits to a displayed owner's keys, values, stretch or
  dimensionality invalidate samples. View, composition, graph type and expression-host presence
  remain part of the key. The unused second warm-hit point copy is removed.
- Any enabled expression, including separated Position followers, keeps whole-document revision
  invalidation because expressions can depend on other layers/compositions. Expression-heavy
  resampling remains a measured bottleneck; this change does not claim to fix it. Editing a
  displayed owner still invalidates the whole graph cache.
- Repeated inline instances share one source row list within an expansion pass. Instance paths,
  times and locks remain separate, and the list is rebuilt next pass, avoiding cross-frame stale
  expansion state.
- The two graph cache slots retain displayed layer owners until replaced or their context is
  destroyed. The per-pass row cache retains unique source lists until the pass ends. These are
  references/shared structures, not duplicated document content. Existing 32-level/cycle guards
  and the 20,000 projected-row limit remain; that output limit is not a strict allocation cap.

The two profiling probes were executed successfully. New behavioral regressions were written and
compiled, not executed, under the fork's daytime policy. Final engine/UI all-targets native check,
strict Clippy and focused WASM check passed; WASM warnings were in unchanged code. Native/browser
interaction QA, input-to-visible-frame latency and pixel comparisons remain unmeasured. No full CI.
