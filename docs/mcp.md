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
| `render_preview` | `comp?`, `time?`, `max_side?`, `path?`, `inline?`, `transparent?` | PNG and frame information |

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

Unknown top-level tool argument keys return JSON-RPC `-32602` naming the key and accepted
arguments. EffectCraft already rejects unknown engine command parameters; that behaviour is
preserved. Tool failures (including escaped panics) return `isError: true`. Malformed JSON
returns `-32700` with a null id and the session keeps serving. The MCP backend owns its session
without a mutex; the engine's render-job mutexes already recover poisoned locks.

## Structured results

Sessions that negotiated MCP 2025-06-18 (and modern per-request clients) get each JSON tool
result as `structuredContent` as well as the JSON text block older clients read. It is always an
object: an array result is `{"items": [...]}`, a scalar `{"value": v}`, and an image reply carries
its metadata (`comp`, `time`, `width`, `height`, `path?`). Tools with a stable shape declare it as
`outputSchema` in `tools/list`: `get_state`, `doc_inspect`, `render_preview`, `undo`, `redo`,
`history`, `command_batch`, `batch` and `save_project`. Earlier revisions see neither field.
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

## Progress and cancellation

Headless `command_run` / `execute_command` calls to `renderQueue.render` (unless `wait: false`)
run the existing background job while the MCP server keeps reading stdin. A
`params._meta.progressToken` string or number opts into `notifications/progress`, at most ten
per second, strictly increasing with a queue-item total. No token means no notifications.
`ping`, inspection and other requests are served while rendering. The existing engine refuses
a second render while one is active. Closing stdin lets the pending render finish and reply.

Send `notifications/cancelled` with `params.requestId` to stop the matching request at the next
frame batch. No response is sent for that cancelled request. Unknown and completed ids are
ignored. The exporter records the exact files it opens, including storage overflow paths, and
removes those files on cancellation; it never scans for similarly named files. Finished earlier
queue items and skipped existing frames remain. Custom exporters implement the same cleanup
contract. A failed queue item gives `isError: true`.

Bridge calls, `wait: false`, batch tools and single-frame tools retain synchronous/polling
behaviour and do not report MCP progress or cancellation. Use a direct blocking command call
for an MCP-cancellable render.
