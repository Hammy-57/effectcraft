# Agent builds on this fork (eliasx45/effectcraft)

Fork-only rules. They are not part of upstream (`storytold/effectcraft`); keep this folder out of
upstream pull requests. Where they differ from `CLAUDE.md` / `AGENTS.md` / `docs/testing.md`, they win
on this fork.

## Why

The owner's machine is one M1 Max (10 cores, 64 GB), shared by many agents and sessions at once.
On 10 October 2026, ten agents each running full builds and tests put about 51 compile jobs on the
10 cores (load average 114): every build took roughly ten times longer, and some agents waited
hours. sccache hit 8 of 2,122 compilations (0.4 %), so each worktree compiled the whole dependency
tree from scratch. The fix is to build less per agent, and to make each build cheaper.

## The rules

**While you iterate (every agent):**
- Use `cargo check -p <crate>` (add `--all-targets` when you touch tests) to see whether it compiles.
  It skips code generation and linking, so it's several times cheaper than a build.
- Run only the tests you wrote or touched: `cargo test -p <crate> --lib <filter>`, or one
  integration test binary with a filter (UI tests: one binary, filter by file, see below).
- Cap your jobs: `CARGO_BUILD_JOBS=4` (or `-j 4`). Several agents share 10 cores; more jobs than
  cores make everyone slower, not faster.
- Don't run `cargo xtask ci`, `cargo xtask ci --affected`, a whole crate's test suite or a release
  build per change.

**Before you hand back:** `cargo fmt`, `cargo clippy -p <crates you touched> --all-targets -- -D warnings`,
and your focused tests. Report exactly what you ran.

**Once, after merging (the coordinating session):** one `cargo xtask ci --affected` on the merged
`main` (or on the integration branch before it lands). That's the "combine, then build once"
step: failures that only show up together surface here, and get fixed before anyone builds on them.
The full `cargo xtask ci` stays for releases.

## Cheaper builds

In progress (AG2.16); this section is updated when it lands.

- UI tests (`crates/ui-egui/tests/*.rs`) compile into **one** test binary instead of one binary per
  file (each of which linked the whole app). Run one file's tests with
  `cargo test -p effectcraft-ui-egui --test ui <file_name>::` (for example `ui_viewer::`).
- Dev and test builds carry line tables only (`debug = "line-tables-only"`; dependencies none):
  panics still show file and line, and linking is faster.
- Later: splitting `crates/engine` (about 68,000 lines in one crate; any edit recompiles it and
  everything above it) would cut rebuilds further.

## Later: a cloud build machine

For large agent fleets, run the agents and CI on a cloud Linux machine with 32–64 cores, and keep
the Mac for the app, After Effects and UI checks. This goes with the plan to speed up CI in the
cloud.
