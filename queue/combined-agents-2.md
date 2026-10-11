title: feat(agents): one test binary per crate, project import and libraries, the agent exam, design lint and motion summary
branch: upstream/agents-lint-motion
depends: upstream/agents-control-robustness

The second of three PRs for AI agents. It builds on the MCP / control-channel PR; once that one is
merged, this diff is only the parts below. Most of its lines are the exam's task JSON.

## What
**Tests**
- Crates with many integration-test files build them as one binary: `ui-egui` (`--test ui`), `host`,
  `export` and `effects`. `tests/<name>/main.rs` declares each `tests/*.rs` with `#[path]`;
  `every_test_file_is_a_module` fails when a file is left out. Tests that create a GPU device hold
  `gpu_lock()` (wgpu's GL backend crashes with concurrent devices). `profile.dev` uses
  line-tables-only debug info for workspace crates.
- UI panel fingerprints: a compact fingerprint of every panel's widgets, pinned in
  `ui_fingerprints.txt`, so an unintended UI change shows up as a test diff.

**Import a project, libraries**
- `file.importProject` (File ▸ Import ▸ EffectCraft Project..., and `.ecproj` / `.ecprojx` given to
  File ▸ Import): merges another project into a folder named after the file, every id shifted past
  the open project's, in one undo step; relative footage resolves next to the imported file. Id
  offsets saturate on damaged files.
- `library.*` commands (generic, off until a folder is added): folders indexed by a `library.json`
  hold reusable comps, presets, footage and sounds; `library.search`, `library.insert`,
  `library.addFolder` / `removeFolder` / `folders` / `get`. Docs: `docs/libraries.md`. No library
  ships with this change.

**The agent exam**
- `evals/agent/`: 21 motion-design tasks (title-safe, lower third, stagger fade, track matte, reframe
  vertical, ...) with a JSON schema; `exam.check {task | checks}` scores a project against a task's
  checks (each one read-only query); `cargo xtask agent-exam <--list|--validate|--check P --task
  T|--run ...>`.

**Design lint and motion summary**
- `render.lint` / MCP `lint_frame`: plain-language design problems at a time or over a range: text
  too small or low contrast (WCAG ratio against a text-free render), outside title or action safe,
  cut off, overlapping text, layers switched on but hidden, keyframes just off the music's beat (a
  small onset/tempo tracker).
- `comp.motion` / `layer.motion` / MCP `get_motion`: how layers move over a range without rendering:
  per-layer segments (enter, exit, move, hold, scale, rotate, fades), off-screen intervals, a
  one-sentence summary and overlaps. Two exam tasks use them end to end.

## Why
The ui-egui tests linked the whole app about 50 times; one binary builds once, and fingerprints
catch accidental UI changes without screenshots. Reusing work across projects needs project import
(After Effects imports a project into a project the same way). Agents cannot judge a frame by eye:
lint and motion summaries give them checkable facts about legibility, layout and choreography, and
the exam is a repeatable measure of how well agents drive the app (a task they keep failing points
at a missing command, a confusing parameter or an unclear reply).

## How tested
- `rustfmt --check` and `cargo check` of the affected crates on this branch.
- On the top of the three-PR series: `cargo clippy --all-targets -- -D warnings` for automation,
  engine, cli, ui-egui, effectcraft, host, export, effects, project, raster, script, gpu and xtask:
  clean. `cargo test`: automation 77/77, cli (unit 3/3, cli 18/18, mcp tests), engine lib 596
  passed (2 ignored), raster, host and xtask pass.
- Not run: the `ui-egui` integration tests (`--test ui`) and the full `cargo xtask ci` (the runner
  had no GPU and 4 cores).

Cherry-picked with `-x` trailers and adapted to current `main`.
