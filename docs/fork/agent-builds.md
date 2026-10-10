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
- Don't set `RUSTC_WRAPPER=sccache`: sccache is off on this machine (it only hit for rebuilds in
  the same target dir, and every compile queued behind its one server).
- Cap your jobs: `CARGO_BUILD_JOBS=4` (or `-j 4`). Several agents share 10 cores; more jobs than
  cores make everyone slower, not faster.
- Don't run `cargo xtask ci`, `cargo xtask ci --affected`, a whole crate's test suite or a release
  build per change.

**Before you hand back:** `cargo fmt`, `cargo clippy -p <crates you touched> --all-targets -- -D warnings`,
and your focused tests. Report exactly what you ran.

**Landing your work:**
1. Commit on your branch.
2. Merge the latest `main` into your branch **once**, at the end (merge, don't rebase: a rebase
   replays every commit and, when `main` moves again, means another rebuild). Then `cargo check`
   the crates you touched: no full build, no test suite.
3. Merge into `main`, then delete your worktree, branch and `target/agent-<name>`.
4. Stop there. **Don't build the app or the release CLI after merging**, and don't ask another
   session to: see "Building the app" below.

**CI only when Elias says so.** No agent runs `cargo xtask ci` or `ci --affected` on its own,
not even the coordinating session after merging. Elias starts CI himself, often on another
computer.

**The watchdog** (`.github/workflows/watchdog.yml`, set up at Elias's request) runs `cargo xtask ci
--affected` on a free GitHub-hosted Linux runner after every push to `main`. It never blocks a push.
A failure shows as the red ❌ on the commit (`gh run list --workflow watchdog.yml`) and in GitHub's email;
with issues turned on it also opens one `watchdog` issue that the next green run closes. If main is
red, fixing it comes before new work.

## Building the app

Nobody builds the app after a merge. It's built when it's opened, once, however many merges
happened in between:

- **Dock:** `~/Applications/EffectCraft.app` (pinned to the Dock). A click opens the app at once
  when nothing changed. When the code changed it shows "Building EffectCraft…" with a Stop button
  and opens the app when the build is done. Stop, closing that window, Cmd-Q or Quit from the Dock
  stop the build (the next click carries on where it stopped). A running app is never quit or
  replaced: the click brings it to the front.
- **Command line:** `scripts/fork/app.sh` (same thing; `--status` prints `running` / `stale` /
  `ready`, `--build-only` builds without opening).
- It builds with the `local` profile (release speed without link-time optimisation, parallel and
  incremental codegen) in `target/local`, one build at a time (a lock), so a rebuild only recompiles
  what changed. Published releases still use `release`.
- After changing `scripts/fork/launcher.swift`, reinstall with `scripts/fork/install-dock-app.sh`.

## Cheaper builds

Landed in AG2.16 (details: docs/testing.md, "One test binary per crate").

- UI tests (`crates/ui-egui/tests/*.rs`) compile into **one** test binary instead of one binary per
  file (each of which linked the whole app). Run one file's tests with
  `cargo test -p effectcraft-ui-egui --test ui <file_name>::` (for example `ui_viewer::`). The same
  goes for `effectcraft-host`, `-export` and `-effects` (`--test host`, `--test export`,
  `--test effects`). A new test file needs its `#[path = "../x.rs"] mod x;` line in
  `tests/<binary>/main.rs`; a test fails until it has it.
- Dev and test builds carry line tables only (`debug = "line-tables-only"`; dependencies none):
  panics still show file and line, and linking is faster.
- Later: splitting `crates/engine` (about 68,000 lines in one crate; any edit recompiles it and
  everything above it) would cut rebuilds further.

## Later: a cloud build machine

For large agent fleets, run the agents and CI on a cloud Linux machine with 32–64 cores, and keep
the Mac for the app, After Effects and UI checks. This goes with the plan to speed up CI in the
cloud.
