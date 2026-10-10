# Agent exam

A benchmark of motion-design tasks that an AI agent performs through the EffectCraft MCP server
(`effectcraft-cli mcp`), scored by deterministic checks. It measures how well agents can drive
EffectCraft end to end: lower thirds, title cards, logo animations, easing, parenting, precomps,
shape layers, effects, masks, track mattes, fixing layout problems, editing and reframing existing
projects.

The full run (`cargo xtask agent-exam --run`) calls a model for every task. It is meant for a
**nightly cloud job**, not for every change; `--validate` and the engine's checker tests are the
cheap gates.

## Layout

```text
evals/agent/
  README.md          this file
  schema.json        JSON Schema of a task file
  tasks/<id>.json    one task per file (the file name is the task id)
```

## Running it

```sh
cargo xtask agent-exam --list                 # tasks with titles and check counts
cargo xtask agent-exam --validate             # parse every task, check its shape (no CLI, no model)

# Score a finished project against one task (exit 1 when it fails):
cargo xtask agent-exam --check work.ecproj --task logo-fly-in [--json]

# Run the exam: build each starting project, let Claude Code do the task over MCP, score it.
cargo xtask agent-exam --run [--tasks logo-fly-in,fade-out] [--out DIR] \
    [--minutes N] [--max-tool-calls N] [--model M]
```

- The CLI is `$EFFECTCRAFT_CLI` if set, else `target/release/effectcraft-cli` (or under
  `$CARGO_TARGET_DIR`), else `cargo run -q --release -p effectcraft-cli --` (`--run` builds the
  release CLI once instead, since the MCP config needs a binary).
- `--check` runs `effectcraft-cli exec exam.check --project P --params '{"task": …, "base": …}' --json`
  (`base` is the task file's folder: relative reference paths resolve against it).
- `--run`, per task, in `<out>/<id>/`:
  1. builds `start.ecproj` from the task's `setup` (`effectcraft-cli run <cmd> <json>… --empty
     --save-as start.ecproj`) or copies its `project`, and copies it to `work.ecproj`;
  2. writes `mcp.json` with one server, `effectcraft`: `effectcraft-cli mcp --project work.ecproj`;
  3. runs Claude Code headless in that folder:
     `claude -p "<prompt>" --mcp-config mcp.json --strict-mcp-config --output-format stream-json
     --verbose --max-turns <toolCalls+2> --allowedTools "mcp__effectcraft__*" [--model M]`. The
     prompt is the task's `prompt` plus how to work: the project is open, use the given names, and
     **save with `save_project`** (to `work.ecproj`) when done; unsaved work is not graded.
     `AGENT_EXAM_CLAUDE` replaces the `claude` binary; `AGENT_EXAM_CLAUDE_ARGS` replaces the flags
     after `-p <prompt>` (`{mcp_config}`, `{max_turns}` and `{model}` are filled in);
  4. stops the agent at the wall-clock limit (`limits.minutes` or `--minutes`), counts tool calls
     (`tool_use` blocks in the `stream-json` transcript, kept as `stream.jsonl`), and scores
     `work.ecproj` with `--check` (kept as `check.json`).
- Output: `<out>/report.json` (summary + per task: id, pass, score, minutes, toolCalls, failures
  with reasons, error) and `<out>/report.md` (pass rate, mean minutes and tool calls, a table, the
  failures). Default `<out>`: `target/agent-exam/<UTC timestamp>`. The report is rewritten after
  each task, so an interrupted run still leaves one.

## Scoring

A task **passes** when every check passes and the agent stayed within `limits.toolCalls`. Its
**score** is the fraction of checks that passed (0–1). A run reports the pass rate, the mean score,
the mean minutes and the mean tool calls. Checks are deterministic facts read from the saved
project, never a model's judgement; prefer property, layer, motion and lint facts over pixel
comparisons.

## Task format

```json
{
  "id": "logo-fly-in",
  "title": "Logo flies in from the left",
  "prompt": "In the comp \"Main\", animate the layer \"Logo\" so it …",
  "setup": [{"command": "comp.new", "params": {"name": "Main", "width": 1920, "height": 1080}}, …],
  "limits": {"minutes": 10, "toolCalls": 40},
  "checks": [ … ],
  "solution": [{"command": "prop.addKey", "params": {…}}, …]
}
```

| Field | |
|---|---|
| `id` | lowercase letters, digits, dashes; the file name is `<id>.json` |
| `title` | one line |
| `prompt` | what the agent is told: plain English, self-contained, naming every comp and layer the checks use |
| `setup` | `null` or engine commands (`{command, params}`) run in a fresh empty session to build the starting project. Prefer this to committed projects |
| `project` | instead of `setup`: a `.ecproj` path relative to the task file |
| `reference` | optional media path for `compare` checks. Only generated or own media (AGENTS.md §1): describe how it is produced, e.g. rendered with `effectcraft-cli render-frame` from a reference setup |
| `limits` | `{minutes, toolCalls}` |
| `checks` | the graded facts (below) |
| `solution` | optional reference solution (engine commands). Never shown to the agent; the engine test `tests_exam::every_task_is_solvable` runs `setup`, checks that the start does **not** pass, runs `solution`, and checks that every check passes, so each task is proven achievable and deterministic |

## Checks

`exam.check {task | checks, base?}` runs the checks against the current project and returns
`{passed, score, total, passedCount, results: [{id, desc, pass, reason, actual}]}`. A failing
check's `reason` says what was expected and what was found; `actual` is the value it saw. A
malformed check fails with a reason (`malformed check: …`); it never fails the whole run. At most
200 checks run at once.

Every check has an `id`, a `desc` and exactly one kind.

### Generic: a query and expectations

```json
{"id": "logo-exists", "desc": "a layer named Logo",
 "query": "comp.info", "params": {"comp": "Main"},
 "select": {"array": "/layers", "where": {"name": "Logo"}},
 "expect": [{"path": "/parent", "op": "exists"}, {"path": "/type", "op": "eq", "value": "solid"}]}
```

- `query` must be a **read-only query command** (`CommandSpec.journal == false`). Commands that edit
  the project are refused, and so are query-like commands with side effects (`engine.batch`,
  `journal.export`, `help.exportRepro`, `file.importFigma`, `figma.setToken`, `render.frame` and
  `render.contactSheet`, which write PNGs, and `exam.check`) and file-writing or file-system
  parameters (`render.compare` `diff_path`, `audio.analyze` `path`, `mediaBrowser.list` `path`).
- `params` are checked like any agent call (unknown keys are an error).
- `select` (optional): one step or a chain of steps, each `{array: "/pointer", where?: {field:
  value, "/pointer": value}, index?: n}`. It picks the `index`-th (0-based, default first)
  element of that array whose fields equal `where`; the expectation paths are then relative to it.
  An expectation may carry its own `select`, applied after the check's.
- `expect`: every expectation must hold. `path` is a JSON Pointer (RFC 6901, `""` = the whole
  value), or a list of pointers (the first present, non-null one is used).
- `show` (optional): a pointer whose value is reported as `actual` when the check fails.

| op | passes when |
|---|---|
| `eq` / `ne` | equal / not equal (numbers by value: `1` = `1.0`; arrays and objects element-wise) |
| `approx` | a number within `tol` (default 0.001); arrays element-wise, and an expected array may be shorter (`[960, 540]` matches `[960, 540, 0]`) |
| `gt` `gte` `lt` `lte` | number compares |
| `contains` | strings: case-insensitive substring; arrays: an element equal to `value` (objects match when they hold every field of `value`; strings case-insensitively) |
| `matches` | a string matches a case-insensitive glob (`*`, `?`) |
| `exists` / `absent` | the path is present and not null / missing or null |
| `len` `lenGte` `lenLte` | length of an array, string or object |
| `in` / `notIn` | the value is / is not one of the `value` array |
| `any` / `all` / `none` | an array has at least one / only / no elements meeting every expectation in `value` (one expectation or a list; paths relative to the element). `all` of an empty array passes: pair it with `lenGte` |

`length: true` on any expectation tests the length of the value instead of the value
(`{"path": "/layers", "length": true, "op": "gt", "value": 3}`).

### Shorthands

Each expands into a generic check (`crates/engine/src/exam.rs`).

| Shorthand | Expands to |
|---|---|
| `{"prop": {"comp"?, "layer", "path", "time"?, "field"?}, "op", "value", "tol"?}` | `prop.get`; tests what renders (`/evaluated` when there is an expression, else `/value`) at `time` (comp seconds, default the current time). `field` tests another part of the reply instead: `keys`, `animated`, `expression` |
| `{"layerCount": {"comp"?}, "op", "value"}` | `comp.info`, the length of `/layers` |
| `{"comp": {"name"}, "width"?, "height"?, "fps"?, "duration"?, "tol"?}` | `comp.info {comp: name}`: the comp exists with those settings (`fps` and `duration` within `tol`, default 0.01) |
| `{"lint": {params}, "maxSeverity"?, "allow"?}` | `render.lint`: no problem worse than `maxSeverity` (`info`, the default, means clean: no errors or warnings; `warning` allows warnings; `none` allows nothing), ignoring the rules in `allow` |
| `{"motion": {params}, "layer", "segment": {"kind", "edge"?, "direction"?, "startApprox"?, "endApprox"?, "tol"?}}` | `comp.motion` for that layer: a segment of that kind exists (start/end within `tol`, default 0.1 s). `segment` may be a list (each must exist) |
| `{"compare": {"reference", "comp"?, "time"?, "threshold"?, "minPsnr"?, "maxSide"?, "maxChangedPct"?}}` | `render.compare` of the rendered frame with the reference image: every pixel within `threshold` levels, or PSNR ≥ `minPsnr`, or at most `maxChangedPct` % of pixels changed |

## Writing a task

- Name every comp and layer the checks read in the prompt ("a text layer named `Title`").
- Check outcomes, not methods: an agent may use any command, expression or keyframe layout that
  gets the result. Use tolerances for positions and times (a frame is 1/fps s).
- Give a `solution` and run `cargo test -p effectcraft-engine --lib tests_exam::` and
  `cargo xtask agent-exam --validate`.
- Media: only what a setup generates (solids, shapes, text, synthetic audio). No external assets.
