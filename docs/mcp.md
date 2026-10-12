# MCP conventions

Run `effectcraft-cli mcp` for a headless session, `effectcraft-cli mcp --ui` for the full app
offscreen (no window), or `effectcraft-cli mcp --bridge 9877` to use the desktop app's control
channel (`--bridge` without a port uses
`EFFECTCRAFT_CONTROL_PORT`, else 9877).

`--ui` and `--bridge` both route every tool through control-channel requests
(`docs/control-protocol.md`) behind the automation crate's `ControlTransport` trait: a TCP
`BridgeClient`, or the CLI's offscreen transport, which owns `effectcraft_ui_egui::offscreen`'s
UI thread. That thread steps the egui_kittest harness until each request is answered and the UI
has settled, feeds the app's synthetic input as real events and renders screenshots with wgpu.
`screenshot` and the `ui_*` / `control` tools are listed only in these two modes; the
`initialize` title names the mode (`headless`, `headless-ui`, `bridge`). The offscreen app's
session is in the server's process, so a transport can also run code on it
(`ControlTransport::on_session`, run on the UI thread between frames): `mcp --ui --autosave`
checkpoints the project that way, like headless `--autosave` ([agents.md](agents.md)).

## Confining file access

`--automation-read-root DIR` and `--automation-write-root DIR` (each repeatable, as in
PhotoCraft) confine what the agent can reach: files are read only under a read or write root and
written only under a write root; with no write root nothing is written. Every engine command
(`execute_command`, `command_run`, each `batch` step) is checked before it runs: its file
parameters must be absolute paths inside the roots (symbolic links resolved, no `..`), and so must
any other absolute path in its parameters. Save, Increment and Save and rendering the Render
Queue are checked against the files they write (the open project's, the queued outputs). Scripts
(`run_script`, `file.runScript`…), Execute File, Reveal in Finder, `$N` references to file paths
in a batch, and the bridge's UI-driving tools (`ui_click`, `ui_key`, `ui_type`, `ui_drag`,
`ui_set`, `script_ui`, `control`) are refused while confined. Without the flags nothing changes.

The server provides the same core conventions as FilmCraft #28:

| Tool | Arguments | Result |
| --- | --- | --- |
| `command_list` | `filter?`, `enabled_only?`, `schemas?`, `brief?` (ids + labels only) | Command catalog |
| `command_run` | `id`, `params?` | Engine command result |
| `command_batch` | `steps: [{id, params?}]`, `stop_on_error?` | `completed`, `failed`, per-step `results` |
| `doc_inspect` | `full?`, `offset?`, `limit?` | Project overview and active composition (layers compact and paged, as `get_comp`) |
| `render_preview` | `comp?` / `item?`, `layer?`, `time?`, `max_side?`, `region?`, `overlays?`, `path?`, `inline?`, `transparent?` | PNG and frame information |
| `render_frames` | `times?` / `count?`, `start?`, `end?`, `cols?`, `max_side?`, `labels?`, `gif?`, `path?` and the render arguments | One labelled contact-sheet PNG and `{times, frames, cols, rows, cell}` |

To check visual work, `render_snapshot {name, comp?, time?}` keeps a frame and
`render_compare {a, b, threshold?, min_psnr?, max_side?, region?, diff_path?, inline?}` compares snapshots,
image files or frames rendered now, returning the metrics and `diffBounds` (box of the changed pixels, or null) as
JSON and, when something changed, the diff heatmap as image content (longest side 768; `diff_path` writes it full size). Both are
annotated like file writers (not read-only, not destructive, idempotent): they never change the
project. See [agents.md](agents.md#verify-your-change-snapshot--edit--compare).

### Compact, paged reads

Clients often cap a tool result (8k characters is common), so the read tools answer compactly and
page long lists. `get_comp` (and `doc_inspect`'s active comp) gives `layerCount` and each layer as a
row of `index`, `id`, `name`, `type`, `in`, `out` plus only what isn't the default: `start` (≠ 0),
`stretch` (≠ 100), `parent`, `trackMatte`, `blendMode` (≠ Normal) and `switches` tags (`hidden`,
`3d`, `solo`, `locked`, `shy`, `adjustment`, `quality:Draft`, …), about 100 characters a layer:

```json
{"blendMode":"Multiply","id":52,"in":0.0,"index":1,"name":"Lower third","out":10.01,"parent":4,"switches":["hidden","3d"],"type":"Solid"}
```

Lists are paged with `offset` / `limit` (defaults: 200 layers, 100 keyframes for `get_property`,
150 properties for `get_layer {flat: true}`). Every paged reply carries its total (`layerCount`,
`keyCount`, `total`), `returned` and `next_offset` (null when nothing follows); a cut list also
carries `truncated: {returned, total, next_offset, hint}`. `get_property` keys are
`{time, value, compTime?, in?, out?}` (interpolation omitted when Linear, values rounded to 4
decimals). A `get_layer` tree over 8,000 characters carries a `hint` to use the flat list.
`full: true` (on `get_comp`, `doc_inspect`, `get_property`) returns the long form unchanged.

`command_batch` stops at the first error by default; false continues. Each edit has its
own undo step. The existing `batch` tool retains its atomic undo grouping and result references.
See [agents.md](agents.md) for the full catalog. Each tool has a title and read-only, destructive,
idempotent and open-world hints. Tools with an optional output path are conservatively annotated
as file writers.

Four older names duplicate a core tool. They are left out of `tools/list` but stay callable
with their own arguments, so existing workflows keep working; `effectcraft-cli mcp --all-tools`
lists them too (described as aliases):

| Hidden alias | Canonical tool |
| --- | --- |
| `list_commands` | `command_list` (same arguments) |
| `execute_command {command \| id, params?}` | `command_run {id, params?}` |
| `get_project` | `doc_inspect` (its `project` is the same summary) |
| `render_frame` | `render_preview` (same arguments, including `path` and `inline`) |

`render_frame`, `render_preview` and `render_frames` share the engine commands `render.frame` and
`render.contactSheet` (headless and bridged alike): `region: [x, y, w, h]` renders one comp rectangle
at full resolution, `layer` one layer solo, `item` a footage or solid item, and `overlays` draws layer
bounds, anchors, motion paths, safe margins or the grid. Contact sheets hold at most 64 cells;
larger `count`s are capped and reported (`requested`, `note`).

Unknown top-level tool argument keys return JSON-RPC `-32602` naming the key, the closest accepted
one (`command_run {"command": …}` → "did you mean `id`?") and the accepted arguments. EffectCraft already rejects unknown engine command parameters; that behaviour is
preserved. Tool failures (including escaped panics) return `isError: true`, and so does any JSON
result with a `failed` count above zero (a `command_batch` / `batch` with a failed step, a
render whose queue items did not all finish), even though the other steps or items succeeded:
read `results` / `items` for the details. Malformed JSON
returns `-32700` with a null id and the session keeps serving. The MCP backend owns its session
without a mutex; the engine's render-job mutexes already recover poisoned locks.

## Structured results

Sessions that negotiated MCP 2025-06-18 (and modern per-request clients) get each JSON tool
result as `structuredContent` as well as the JSON text block older clients read. It is always an
object: an array result is `{"items": [...]}`, a scalar `{"value": v}`, and an image reply carries
its metadata (`comp`, `time`, `width`, `height`, `path?`). Tools with a stable shape declare it as
`outputSchema` in `tools/list`: `get_state`, `doc_inspect`, `render_preview`, `render_frames`,
`render_snapshot`, `render_compare`, `get_layout`, `analyze_audio`, `undo`, `redo`, `history`,
`command_batch`, `batch` and `save_project`. Earlier revisions see neither field.
Long renders (below) reply with text only.

## Resources

Resources `effectcraft://document` and `effectcraft://commands` return JSON matching
`doc_inspect` and `command_list`. Clients declaring MCP 2026-07-28 in per-request `_meta`
receive `resultType: complete` and list/read cache hints; document reads are never cached.

`resources/templates/list` offers four templates, read with `resources/read`:

| Template | Content |
| --- | --- |
| `effectcraft://comp/{comp}` | `get_comp` JSON |
| `effectcraft://layer/{comp}/{layer}` | `get_layer` tree JSON; `?flat=true` for the flat list |
| `effectcraft://frame/{comp}/{time}` | PNG (`blob`, `image/png`) at comp time `time` seconds, longest side 960; `?max_side=N` (0 = full size, at most 8192) |
| `effectcraft://command/{id}` | `describe_command` JSON |

`{comp}` and `{layer}` take an id (a number), a name, or `#n` for a layer. Each segment is
percent-encoded (`Main%20%2F%20Intro`, `%231`). An unknown scheme, kind, comp, layer or command
returns `-32002`; malformed URIs (wrong segment count, bad escape or time, unknown query key)
return `-32602`.

```json
{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"command_run","arguments":{"id":"comp.new","params":{"width":640,"height":360}}}}
{"jsonrpc":"2.0","id":2,"method":"resources/read","params":{"uri":"effectcraft://document"}}
{"jsonrpc":"2.0","id":3,"method":"resources/read","params":{"uri":"effectcraft://frame/1/0.5?max_side=320"}}
```

## Analysis tools

Headless agents can't look at a render or hear the mix, so read-only tools return numbers.
Each wraps an engine command, so the CLI, scripts (`app.run`) and the control channel get the same.

| Tool | Command | Result |
| --- | --- | --- |
| `get_layout` | `layer.bounds` | Per layer: `visible` + `why`, comp-space `bounds`, `corners`, `center`, `anchorInComp`, `onScreen`, `source`; optional `pixelBounds` and `overlaps` |
| `analyze_audio` | `audio.analyze` | Peak, true peak, RMS, integrated LUFS, clipped samples, silences, channels; optional waveform |

- **Geometry** is `sourceRectAtTime` through the transform, parents and, for 3D layers, the
  active camera. A 3D layer partly behind the camera reports `projected: false` and no bounds.
  The engine function is `effectcraft_engine::layout::layer_geometry`.
- **Audio** is the 48 kHz stereo mix preview and export play. Integrated loudness follows ITU-R
  BS.1770-4 (K-weighting, 400 ms blocks, absolute and relative gates). Levels are null for
  silence, and the true peak is a 4× oversampled estimate. A comp without audio returns
  `hasAudio: false`. One call covers at most an hour.

`analyze_audio {waveform: true}` returns the waveform as PNG content next to the JSON; the server
draws it from the command's min/max columns, so it works the same headless and bridged.

## Notifications

The server writes notifications on stdout after the reply to the request that caused them.

**Resource subscriptions** (`resources.subscribe: true`). `resources/subscribe {uri}` accepts any
readable URI, including template matches (an unknown or malformed one is an error, as for
`resources/read`); `resources/unsubscribe` removes it. When the project's revision changes
(`project.summary.revision`; every edit, undo and redo, never a query or a value set to what it
already was), each subscribed `document`, `comp`, `layer` and `frame` URI gets
`notifications/resources/updated {uri}`. This is conservative: every subscribed project URI is
reported, whichever comp changed. Command URIs never are. Headless sessions compare the revision
after every request. In bridge mode, while anything is subscribed, each request polls
`project.summary` once. Edits made in the app are therefore reported with the next request, not
as they happen.

**Logging** (`logging: {}`). `logging/setLevel {level}` takes an RFC 5424 level (`debug` …
`emergency`). Until then, `warning` and above are sent. Entries are `notifications/message`
with `logger: "effectcraft"` and `data: {message, …}`:

| Level | When |
| --- | --- |
| `warning` | An edit leaves new expression errors in the active comp (`data.errors`, from `expr.errors`; each error is reported once) |
| `warning` | `open_project` finds missing footage (headless; `data.missing`) |
| `error` | A `--autosave` checkpoint failed (also appended to the tool reply) |
| `error` / `info` | Session toasts (headless), such as "saved by a newer EffectCraft" |

In bridge mode only the expression check applies; the app shows its own toasts and footage
checks.

## Prompts

`prompts/list` and `prompts/get` serve workflow recipes. Each one is a single user message that spells
out the tool flow (canonical tool names, the layer-time versus comp-time pitfall, rendering to check):

| Prompt | Arguments |
| --- | --- |
| `title_card` | `text`, `style?` |
| `lower_third` | `name`, `title` |
| `port_ae_script` | `script` (After Effects ExtendScript, run through `run_script`) |
| `check_against_reference` | `reference_path`, `comp?`, `time?` (`render_compare` against the reference) |
| `animate_layer` | `layer`, `motion` |

Argument values are strings. A missing required argument, an unknown argument or prompt, or a
non-string value returns `-32602`.

## Change notifications and retries

The server advertises `resources.subscribe`. After `resources/subscribe {uri:
"effectcraft://document"}`, every tool call that changed the project revision is followed by
`notifications/resources/updated {uri}` (bridge mode reads the app's revision after each call,
so edits made in the window are reported with the next call). `resources/unsubscribe` stops it.

A `tools/call` with `params._meta.requestId` (or a `request_id` argument, which is removed before
argument checking) is idempotent: the same id again returns the first result without running the
tool (last 128 ids). Bridge calls also send a `requestId` on the control channel, so the bridge's
reconnect-and-resend never runs a command twice.

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
everything sent after them wait in line until the job ends, then run in order; a second render
is refused at once instead (see "Requests during a render"). This works
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

### Requests during a render

A render job owns a snapshot of the project taken when it started. While it runs:

- Every way of starting a second render is refused with `isError: true`: a blocking
  `renderQueue.render`, one with `wait: false`, or one inside a batch.
- Other commands, including edits, run normally and change the project. They do not affect the
  running job, which keeps rendering its snapshot; to render an edit, render again after the
  job ends. Queue items are matched by id when the job reports their status, so an item removed
  mid-render is left out of the reply and counted in `failed`.

`renderQueue.render {"wait": false}` returns the ids being rendered at once; poll
`renderQueue.list` for status and stop it with `renderQueue.stop`.

## Reliable inspection and key writes

Compact composition, keyframe and flat property pages retain exact numbers and fit a
7,000-byte serialized JSON budget by default (`max_chars`, 1,024–1,000,000). `limit` is an upper
bound, not a promise: continue from `next_offset` until null and reconcile `returned` against
`total`. The JSON text puts completeness metadata before the list. `full: true` without a
paging/budget argument is an explicit unbounded compatibility escape hatch.

An oversized field is an object with `$omitted: true`, `editable: false`, `pointer` and
`fetch: "get_field"`. It is not the field's value. Call `get_field` with `source` set to the
original inspection tool, the same target/time/flat arguments, and that pointer. Concatenate
its `text` chunks using byte `next_offset` and parse the combined JSON to recover the exact
value. Do not write an omission marker back to a property.

Inspectors identify the composition, revision, comp time and `timeBase: "comp"`. `value` and
`rawValue` are pre-expression values; `evaluatedValue` (and the legacy `evaluated`) is the
expression result, with `expressionError` when evaluation fails. `uid`, `enabled`, and
`effectiveEnabled` distinguish stable identity and disabled ancestor groups.

Prefer `timeBase: "comp"` when keying at a Timeline time. A timed `set_property` response
reports `requestedTime`, actual snapped `layerTime` / `compTime`, and `appliedValue`, and reads
back at that comp time. Script failures set MCP `isError: true` while preserving error details,
output and partial-edit information. Ambiguous name aliases are errors with candidate IDs;
use numeric IDs, canonical property paths or `@uid` to address a specific object.
