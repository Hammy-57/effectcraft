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
3. Merge into `main`, **push `main`** (the nightly cloud run only sees what's on GitHub), then delete
   your worktree, branch and `target/agent-<name>`.
4. Stop there. **Don't build the app or the release CLI after merging**, and don't ask another
   session to: see "Building the app" below.

**CI only when Elias says so.** No agent runs `cargo xtask ci` or `ci --affected` on its own,
not even the coordinating session after merging. Elias starts CI himself, often on another
computer.

**Day and night.** During the day, agents write the regression test but don't run it: `cargo
check` and clippy on the touched crates, then merge, saying in the merge message which tests
weren't run. Every night at 22:00 UTC a cloud routine ("EffectCraft nightly: CI, tests, fixes,
community PRs") does the heavy part. At 22:40 Mac time a LaunchAgent
(`scripts/fork/push-main.sh`, log `~/Library/Logs/EffectCraft push-main.log`) pushes `main` first
in case a merge wasn't pushed: fast-forward only, never other branches. The routine then it merges upstream, runs those tests and the full CI, fixes
main until green, opens or updates focused PRs to storytold for finished work, and leaves a
morning summary.

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
- It builds with `[profile.local]` in the root `Cargo.toml` (inherits `release`; `lto = false`,
  `codegen-units = 16`, `incremental = true`) in `target/local`, one build at a time (a lock), so a
  rebuild only recompiles what changed. Exit status 130 means the build was stopped, 1 that it
  failed (log: `target/local/build.log`). Published releases still use `release`.
- The files: `scripts/fork/app.sh` (the build-and-open logic), `scripts/fork/launcher.swift` (the
  Dock app's window and Stop button) and `scripts/fork/install-dock-app.sh` (builds the launcher
  with `swiftc` into `~/Applications/EffectCraft.app` and adds it to the Dock once). After changing
  the launcher, run the install script again.
- Privacy permissions (Full Disk Access, Files and Folders) belong to `EffectCraft.app`. The
  launcher starts the app itself and stays running, hidden, until the app quits, so one grant
  covers every rebuilt binary. An app started from a terminal or by an agent (`scripts/fork/app.sh`)
  gets that program's permissions instead. Reinstalling the launcher re-signs it: switch
  EffectCraft off and on again in Full Disk Access.

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

## Owner-local icons

On the owner's machine the app shows After Effects' own icons and cursors (layer-source and
Project item icons, the toolbar, the viewer footer, the timeline trim cursor), extracted from the
owner's licensed install into `~/Library/Application Support/EffectCraft/owner-assets` (outside
every checkout; `EFFECTCRAFT_ICON_REFERENCE` points elsewhere). `crates/ui-egui/src/local_assets.rs`
loads them at runtime. This is the owner-approved exception in `AGENTS.md` §1.9: the artwork is
never committed, embedded, shipped or attached anywhere, code names an asset only by file name, and
every icon keeps its code-drawn fallback, so builds without the folder (CI, other machines,
upstream) look as before.
