title: feat(agents): agent exam (exam.check and 21 motion-design tasks)
branch: upstream/agents-exam
depends: upstream/agents-import-project-library

## What
- The agent exam: `evals/agent/` holds 21 motion-design tasks (title-safe, lower third, stagger fade,
  track matte, reframe vertical, ...) with a JSON schema; `exam.check {task | checks}` scores a project
  against a task's checks (each runs one read-only query and tests its result);
  `cargo xtask agent-exam <--list|--validate|--check P --task T|--run ...>` lists, validates, checks
  or runs them.

## Why
A repeatable measure of how well AI agents drive EffectCraft: a task agents keep failing points at a
missing command, a confusing parameter or an unclear reply. Most of the diff is the task JSON.

## How tested
- `rustfmt --check` and `cargo check` of the affected crates on this branch (queue-pr.sh).
- On the top of the stacked series this PR starts or belongs to
  (`upstream/agents-own-undo`): `cargo clippy --all-targets -- -D warnings` for automation, engine,
  cli, ui-egui, effectcraft, host, export, effects, project, raster, script, gpu and xtask; and
  `cargo test` for automation, cli, engine, raster, host and xtask. Results: clippy clean; automation 77/77, cli (unit 3/3, cli 18/18, mcp tests), engine lib 596 passed (2 ignored), raster, host (including tests/repros) and xtask all pass.
- Not run here: the `ui-egui` integration tests (`--test ui`) and the full `cargo xtask ci`
  (the runner used had no GPU and 4 cores).

Cherry-picked from eliasx45/effectcraft (`-x` trailers) and adapted to current upstream `main`:
new modules are declared by hand in `lib.rs` / `commands/mod.rs` (the fork's auto-registration
build step is not part of this series).
