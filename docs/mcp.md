# MCP conventions

Run `effectcraft-cli mcp` for a headless session, or `effectcraft-cli mcp --bridge 9877`
to use the desktop app's control channel. Existing ports are unchanged.

The server provides the same core conventions as FilmCraft #28:

| Tool | Arguments | Result |
| --- | --- | --- |
| `command_list` | `filter?`, `enabled_only?`, `schemas?` | Command catalog |
| `command_run` | `id`, `params?` | Engine command result |
| `command_batch` | `steps: [{id, params?}]`, `stop_on_error?` | `completed`, `failed`, per-step `results` |
| `doc_inspect` | none | Project overview and active composition |
| `render_preview` | `comp?`, `time?`, `max_side?`, `transparent?` | PNG and frame information |

To check visual work, `render_snapshot {name, comp?, time?}` keeps a frame and
`render_compare {a, b, threshold?, min_psnr?, max_side?, diff_path?, inline?}` compares snapshots,
image files or frames rendered now, returning the metrics as JSON and, when something changed, the
diff heatmap as image content (longest side 768; `diff_path` writes it full size). Both are
annotated like file writers (not read-only, not destructive, idempotent): they never change the
project. See [agents.md](agents.md#verify-your-change-snapshot--edit--compare).

`command_batch` stops at the first error by default; false continues. Each edit has its
own undo step. The existing `batch` tool retains its atomic undo grouping and result references.
Existing tools, including `list_commands`, `execute_command`, and `batch`, remain listed
because existing workflows use them; there are no hidden aliases. See [agents.md](agents.md)
for the full catalog. Each tool has a title and read-only, destructive, idempotent and
open-world hints. Tools with an optional output path are conservatively annotated as file writers.

Unknown top-level tool argument keys return JSON-RPC `-32602` naming the key and accepted
arguments. EffectCraft already rejects unknown engine command parameters; that behaviour is
preserved. Tool failures (including escaped panics) return `isError: true`. Malformed JSON
returns `-32700` with a null id and the session keeps serving. The MCP backend owns its session
without a mutex; the engine's render-job mutexes already recover poisoned locks.

Resources `effectcraft://document` and `effectcraft://commands` return JSON matching
`doc_inspect` and `command_list`. Clients declaring MCP 2026-07-28 in per-request `_meta`
receive `resultType: complete` and list/read cache hints; document reads are never cached.

```json
{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"command_run","arguments":{"id":"comp.new","params":{"width":640,"height":360}}}}
{"jsonrpc":"2.0","id":2,"method":"resources/read","params":{"uri":"effectcraft://document"}}
```

## Progress and cancellation

Long calls don't block the server. While one runs, `ping` is answered at once and other
requests are served (see below); `notifications/cancelled` with `params.requestId` stops it, and
no response is sent for the cancelled request. Unknown and completed ids are ignored; a
cancelled request that is still waiting in line is dropped. A `params._meta.progressToken`
string or number opts into `notifications/progress` (`progress`, `total` when known, `message`),
at most ten per second and strictly increasing. Closing stdin lets a running call finish and
reply.

**Background-job commands.** `command_run` / `execute_command` of these commands, when they
block, start their engine job with `wait: false`, watch it through `jobs.list` (renders:
`renderQueue.list`) and reply what the blocking command returns. The table is `SPECS` in
`crates/automation/src/long_job.rs`.

| Command (blocks) | Progress | On cancel |
| --- | --- | --- |
| `renderQueue.render` (unless `wait: false`) | queue items | stops at the next frame batch; the exporter deletes the files it opened for the stopped item; finished items and skipped frames stay |
| `track.analyze`, `track.mask` (`wait: true`) | frames | stops; the frames analysed so far stay, as the one "Analyze Track" / "Track Mask" undo step (as After Effects' Stop) |
| `warp.analyze`, `track.warpStabilizer`, `camera.analyze`, `track.camera`, `camera.deletePoints` (`wait: true`) | frames (per phase) | nothing is written |
| `roto.propagate`, `roto.freeze` (`wait: true`) | frames | propagation keeps the frames it cached (no project change); Freeze writes nothing |
| `contentFill.generate`, `layer.sceneEditDetection`, `footage.check` (`wait: true`) | the task's steps | the task's result is discarded; the project is unchanged |
| `jobs.wait` | finished tasks | only the waiting stops; the tasks keep running |

The job works on a snapshot of the project (or writes its results when it ends, as in the app),
so other requests are served while it runs, including edits. Another long call, a script, and
everything sent after them wait in line until the job ends, then run in order. This works
headless and bridged (`--bridge`: the app runs the job; the server polls it).

**Scripts.** Headless `run_script` runs on a worker thread that owns the session meanwhile, so
other requests wait in line (in order) and `ping` is answered. A cancel or the `timeout`
(seconds, default 300, `0` = no limit; also `script.run {timeout}`) ends the script with an error
`try`/`catch` cannot intercept: checked every ~100k bytecode steps and in every object-model call.
An undo group the script left open is rolled back (Edit ▸ Redo restores it); edits outside undo
groups are kept; the output says which. Code inside a built-in's callback (`Array.forEach`, a
sort comparator) that never calls the object model is only stopped by the loop-iteration limit.
Bridged scripts honour `timeout` (in the app) but can't be cancelled.

Batch tools (`batch`, `command_batch`), single-frame tools, other synchronous commands
(`render.saveCurrentPreview`, `file.importAep`, `layer.autoTrace`, imports without `background`)
and every call with `wait: false` stay synchronous: no progress, no cancellation.
