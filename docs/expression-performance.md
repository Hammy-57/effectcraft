# Expression graph cache capacity and measurements

14 October 2026, M1 Max (10 cores, 64 GiB). This follow-up addresses the expression-heavy
resampling case in [interaction-performance.md](interaction-performance.md). It changes the
expression result memo, not the renderer, document model or graph dependency invalidation.

## Cause and retained change

A 720-point speed curve evaluates an expression twice per point. Twenty curves therefore request
28,800 values. The previous memo cleared at 8,192 entries, even when small results used little of
its nominal 32 MiB budget. Diagnostic sweeps confirmed zero script executions for a repeated
four-curve sweep, but all 28,800 scripts reran for every twenty-curve sweep.

The memo now allows up to 65,536 entries within a **32 MiB estimated per-thread retained-storage
budget**. Accounting includes script/path text, current values, request strings, vector capacities,
recursive responses/results, all path vectors, inline entries and reserved hash-table storage.
The 1 MiB entry and 4,096-dependency limits remain. Current insertion capacity anticipates growth;
high-water capacity accounts for allocated buckets, including after removals. On pressure, entries
are cleared while reusable table storage stays allocated and accounted for; storage is released
when even the empty table plus the new entry cannot fit. A post-insert guard handles unexpected
growth. The estimate is not an allocator-RSS limit and does not include the JS runtime or document.

A validated memo hit also skips unconditional composition/layer/property metadata preparation.
Misses retain that preparation, using a stack array instead of allocating a temporary request
vector. Recorded dependency answers are still resolved and compared before every hit. Recursion,
cycle and execution-budget guards, result conversion and existing impurity restrictions are
unchanged. No broader graph-cache shortcut was added for expressions.

## Measured result

Baseline: local main `e7589cdc`. Both builds used the repository's test profile (UI opt-level 1).
The diagnostic swept 1/4/8/20/50 synthetic sine-expression curves at 720 points each, hashing every
numeric output and counting script executions. The UI probe called the actual graph `curves`
function, including the existing two-key fixture and unrelated opacity edits that invalidate the
graph cache. Three alternating baseline/candidate process pairs were run for each probe, with
warmup excluded: 7 diagnostic or 25 UI timing samples per run. Figures below are medians of the
three run p50/p95 values. No compiler overlapped a trial. After Effects and normal user apps
remained active, so these are shared-machine measurements, not an idle-system certification.

| Case | Baseline p50 / p95 (ms) | New p50 / p95 (ms) |
|---|---:|---:|
| Actual graph: 1 expression, unrelated edit | 2.077 / 2.542 | 0.883 / 1.146 |
| Actual graph: 20 expressions, unrelated edit | 294.347 / 319.031 | 22.175 / 23.970 |
| Diagnostic: 4 curves / 5,760 values | 8.748 / 11.758 | 3.776 / 3.846 |
| Diagnostic: 8 curves / 11,520 values | 119.670 / 127.875 | 7.828 / 7.997 |
| Diagnostic: 20 curves / 28,800 values | 300.970 / 321.803 | 21.020 / 26.662 |
| Diagnostic: 50 curves / 72,000 values | 793.051 / 846.005 | 754.185 / 795.556 |

The 20-curve UI p95 reduction is about **92.5%**. Its repeated diagnostic sweeps execute **zero scripts** after warmup instead of
28,800. All five diagnostic curve counts have identical baseline/candidate numeric checksums.
The 50-curve working set still exceeds the cache: it executes all 72,000 scripts per sweep.
Clearing entries does not cause table growth during the measured sweeps; no acceleration claim is
made for cold or oversized expression workloads based on the small stress-case difference.

Median whole-process peak RSS (`/usr/bin/time -l`):

| Probe | Baseline | New | Increase |
|---|---:|---:|---:|
| Diagnostic, including 50 curves | 27.36 MiB | 57.58 MiB | 30.22 MiB |
| Actual Graph Editor preparation | 53.08 MiB | 82.39 MiB | 29.31 MiB |

The predeclared acceptance gate was at least 50% lower 20-expression p95, matching output
checksums, no more than 32 MiB added whole-probe peak RSS, and small-case regression below the
larger of 10% or 0.1 ms. These gates passed. The extra memory is a real tradeoff; per-thread caches
can add up across render workers. This is not a whole-app RAM, GPU-memory or input-to-visible-frame
measurement. First visits to new sample times still execute expressions. Arbitrary expressions,
linked rigs, image sampling and browser runtime performance are not certified by these fixtures.

Earlier experiments that freed/regrew tables or kept an eviction queue were rejected for memory
and stress-case costs. The final change retains the original clear-on-pressure policy and reuses
its bounded storage. Local raw logs, rejected experiments, environment records and checksums are
under `plan/expression-performance-2026-10-14/` (gitignored).

## Verification

Four new regressions cover the 28,800-entry working set, oversized results/request names,
2,000 insertions through repeated memory-budget clears, and replacement byte accounting. They
and existing expression/UI regressions compile; behavioral test execution remains deferred under
the fork's daytime policy. The explicit profiling probes ran successfully. Source review checked
memory accounting, dependency revalidation, frame/recursion scope and request iteration.

Final strict native all-targets Clippy and focused WASM checks passed; WASM warnings were in
unchanged code. Logs are retained with the landing receipt. No full CI, application rebuild/restart, live project/clipboard access or upstream PR.
