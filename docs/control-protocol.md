# Control protocol

The desktop app exposes a control channel for agents, tests and the MCP bridge:

```sh
effectcraft --control 9877        # or EFFECTCRAFT_CONTROL_PORT=9877
```

The port is `--control`, else `EFFECTCRAFT_CONTROL_PORT`, else Settings ▸ Scripting & Expressions ▸
Control Channel Port (9877) when Let Agents Control EffectCraft is on. The app prints the port it
listens on. A port that is already taken (usually by a second EffectCraft) is reported in the
window's status bar and on stderr: give each app its own port, and point its MCP bridge at it
(`effectcraft-cli mcp --bridge` without a port uses `EFFECTCRAFT_CONTROL_PORT`, else 9877).

- **Transport:** TCP on `127.0.0.1` only, one JSON object per line in each direction. Connections are
  persistent; requests on one connection are answered in order.
- **Request:** `{"id": 1, "method": "engine.execute", "params": {...}}` (`id` is echoed back, `params`
  defaults to `{}`). An optional top-level `"requestId": "<unique string>"` makes retries safe: a
  request whose `requestId` was already seen (on any connection, last 128 ids) gets the first reply
  again and doesn't run twice; while the first is still running the retry gets an error saying so.
  Replies larger than 256 KiB (frames, inspections) aren't kept, so such a retry runs again.
- **Reply:** `{"id": 1, "ok": true, "result": ...}` or `{"id": 1, "ok": false, "error": "message"}`.
- Requests run on the UI thread between frames. Input methods (`ui.click`, `ui.key` and similar)
  reply after the synthetic input has been processed. Methods that need an element that isn't drawn
  yet are retried for a few frames. A command that waits for a background job (`wait: true` on
  `renderQueue.render`, the analyses, `footage.check`, Content-Aware Fill, Scene Edit Detection;
  `jobs.wait`, `render.frame` / `render.contactSheet`, which render on a copy of the project, and
  `file.importAep` / `file.importProject`, which read the file in the background)
  starts the job and replies when it ends, with the same result as the blocking
  command: the UI keeps drawing and other connections' requests run meanwhile. A request times
  out after 60 s. Clients sharing the app take turns: each `client`'s first request runs before
  anyone's second, and one frame runs at most 50 ms of requests (the rest run on the next frame),
  so the window keeps drawing. If the app hadn't started it
  by then, it is cancelled and never runs: the reply has `"notRun": true` and says it is safe to
  retry. If it had started, the reply says it may still complete.
- **Only requests are read:** every line must be a JSON object with a string `method` (blank lines are
  skipped). Anything else (text that isn't JSON, a JSON array or number, an object without `method`,
  invalid UTF-8, a line longer than 4 MiB) gets one error reply ending in "closing the connection", and
  the server closes the connection, so nothing sent after it runs. An HTTP request (for example a web
  page's cross-origin `fetch` to `127.0.0.1:<port>`) therefore can't smuggle a command in its body: its
  request line is rejected first. At most 32 connections are served at once; further ones get an error
  line and are closed. The port has no authentication, so only enable it while you use it.

Quick test:

```sh
printf '{"id":1,"method":"engine.execute","params":{"command":"comp.new","params":{"name":"A"}}}\n' | nc 127.0.0.1 9877
```

## Engine

| Method | Params | Result |
|---|---|---|
| `engine.execute` | `{command, params?}` | The command's result. Every engine command (`effectcraft-cli commands`) runs exactly as it does headless, so it never opens a dialog: `comp.new {}` creates a default comp. UI-only ids (`tool.*`, `view.*`, `window.*`, `playback.*`, `timeline.*`, `app.*`) fall through to the menu dispatcher. Unknown top-level params are rejected, and the error lists the accepted keys. |
| `ui.menu.invoke` | `{id, params?}` | Like a menu click: commands with empty params may open their dialog (New Composition, Solid Settings, file pickers). |
| UI commands (via either) | | `timeline.column {column, visible?}` (av, keys, label, num, comment, switches, modes, parent, in, out, duration, stretch), `timeline.sourceName {value?}`, `timeline.search {query}`, `timeline.reveal.<kind>` / `timeline.revealAdd.<kind>` (the property shortcuts and Shift+shortcut: position, scale, rotation, opacity, anchor, effects, masks, feather, levels, animated, and the double-press sets maskPath, maskOpacity, timeRemap, expressions, material, paint, selected, missingEffects, waveform), `timeline.keyAt.<anchor\|position\|scale\|rotation\|opacity>` (Alt+Shift+A/P/S/R/T), `timeline.twirlSelected` (Ctrl/Cmd+`), `timeline.collapseAll`, `flowchart.options {layers?, effects?, solids?, direction?: lr\|tb, comp?}`, `flowchart.graph {comp?}` → `{nodes, edges}`, `window.workspace {name}` (all After Effects workspaces), `app.fxConsole {query?}` (Edit ▸ Effect Console, Ctrl+Space: a search bar at the pointer, `fxConsole.search`, results `fxConsole.result.<i>`; Enter applies the effect or preset to the selected layers). |
| `engine.commands` | `{filter?, enabledOnly?}` | `[{id, label, menu, shortcut, params, enabled, why}]` |
| `ui.menu.list` | `{}` | The menu tree with enablement. |
| `render.frame` | `{comp? \| item?, layer?, time? \| frame?, max_side?, transparent?, region?, overlays?, path?, base64?}` | Renders a frame through the session, independent of the viewer zoom or resolution; the same code as the engine command `render.frame`. `comp` is an id or name (default: the active comp), `time` is in comp seconds (default: the CTI), `max_side` caps the longest side (0 = full size). `item` renders a footage or solid item on its own (source time), `layer` one layer solo, `region: [x, y, w, h]` one comp rectangle at full resolution (clipped to the frame; `max_side` applies to it), and `overlays` (`true` = bounds + anchor, or a list of `bounds`, `anchor`, `motionPath`, `safeMargins`, `grid`, `all`) draws the viewer's overlays and adds `layers: [{id, index, name, bounds: [x, y, w, h], anchor, keyPositions?}]` (comp space, as `layer.bounds` measures them). Writes a PNG to `path` (default: a temp file) and returns `{comp \| item, time, frame, width, height, scale, region?, path}`. With `base64: true` it returns `{…, png: "<base64>"}` (and still writes `path` when one is given). Several frames on one labelled sheet: `engine.execute` `render.contactSheet {times? \| count?, start?, end?, cols?, max_side?, labels?, gif?, …}`. |

Useful query commands for `engine.execute` (they aren't journaled or undoable): `project.summary`,
`comp.info {comp?}`, `layer.tree {layer, comp?, depth?, time?}` (every node has a `path`),
`prop.get {layer, path, comp?, time?}`, `editor.state`, `command.list {filter?, enabledOnly?, schemas?}`,
`command.describe {command}` (params doc and JSON Schema), `app.capabilities` (version, counts,
export formats, parity summary), `jobs.list` (Progress panel jobs, e.g. the background
`footage.check` after File ▸ Open), `render.snapshot {name, comp?, time?, max_side?, transparent?}`
(keep a rendered frame in the session, 16 at most), `render.snapshots {clear?}` and
`render.compare {a, b, threshold?, min_psnr?, max_side?, region?, amplify?, diff_path?, diff_inline?}`
(`a`/`b`: a snapshot name, `{snapshot}`, `{path}` of a PNG/JPEG, or `{comp?, time?, transparent?}`
rendered now) → `{width, height, max_abs, mean_abs, alpha_max_abs, psnr_db, ssim, changed_pixels,
changed_pct, pass, diffBounds, region?, diff_path?, diff_png?}` (`region: [x, y, w, h]` compares
one rectangle; `diffBounds` boxes the changed pixels or is null; metrics and size rules in
[agents.md](agents.md#verify-your-change-snapshot--edit--compare)). Analysis queries (see
[mcp.md](mcp.md#analysis-tools)): `layer.bounds {comp?, time?, layers?, pixels?, overlaps?}`
(layer geometry in comp space) and `audio.analyze {comp?, start?, end?, layer?, waveform?, path?}`
(peak, RMS, LUFS, clipping and silences of the mix). Design lint: `render.lint {comp?, time? | start?, end?, step?, rules?, minTextPx?, minContrast?}`
→ `{comp, compName, times, truncated, clean, counts {error, warning, info}, problems: [{rule, severity,
layer, layerName, time, until, message, other?}], notes}` (rules and how to read it: [agents.md](agents.md#design-lint)).
Motion summaries: `comp.motion {comp?, layers?, start?, end?, step?}` /
`layer.motion {comp?, layer, start?, end?, step?}` → per layer `segments` (enter, exit, move,
hold, scale, rotate, fade-in, fade-out), `offscreen` intervals and a `summary` sentence, plus
`overlaps` ([agents.md](agents.md#motion-summary)).
Agent exam: `exam.check {task? | checks?, base?}` scores the
project against an exam task's checks (each runs one read-only query and tests its result) →
`{passed, score, total, passedCount, results: [{id, desc, pass, reason, actual}]}`
([evals/agent/README.md](../evals/agent/README.md)).

`project.summary` and `editor.state` include the document `revision` (bumped on every change, undo and
redo included). Every command's `params` also accepts `expectRevision` (the command doesn't run and
errors with `document changed (revision X, expected Y)` when the document moved on) and `client` (a
driver name: its undo steps are tagged, and `edit.undo {"client": …}` refuses unless that client made
the last step). Both are handled by `Session::execute` and stripped before the command sees its params.
A number always names an id (layer, effect, mask…), never a position; use `"#n"` for the n-th.

Every engine command is reachable through `engine.execute`, and every interactive widget of every
panel registers an element id; both are enforced by `crates/ui-egui/tests/ui_agent_audit.rs`.

### Clients: cursors and claims

A request whose params carry `client` runs on that client's own cursor: active comp, current times,
selected layers / properties / keys / mask vertices / Project items, the current track and on-canvas
text editing (`Session::execute` swaps it in for the command and the user's back afterwards; batch and
script steps inherit it). A new client starts from a copy of the user's cursor; at most 64 are kept
(least recently used dropped). Open comp tabs, clipboards and tool and viewer options stay shared; a
client opening a comp adds its tab but sends no `OpenComp` to the UI. `editor.state` returns the
caller's view and adds `client` (when given), `clients` (`[{client, active_comp, time, selected_layers}]`,
most recent first) and `claims`.

| Command | Params | Result |
|---|---|---|
| `comp.claim` | `{comp?, ttl?}` + `client` (required) | `{comp, name, client, ttl, renewed, warnings?}`; `ttl` seconds, default 300, capped at 86400; the owner re-claiming renews; another client's live claim is an error naming it |
| `comp.release` | `{comp?, force?}` | `{released: [{comp, name, client}]}`; with `client`: that comp (only if you own it) or all your claims; without `client` only with `force: true` (anyone's, one comp or all) |
| `comp.claims` | `{}` | `{claims: [{comp, name, client, expiresIn, ttl}]}` |

While a claim is live, a command from another client that changes the claimed comp (layers, properties,
name) or deletes it fails with `composition "<name>" is claimed by <owner> (expires in 4m 12s); …` and
changes nothing; edits are checked in `Session::edit`, and anything else (undo, redo, history jumps)
by comparing the comp before and after the command. Requests without `client` are never refused.
Expired claims are dropped when next checked; opening another project drops all claims and cursors.
The TCP control server counts the open connections per `client` and runs `comp.release {client}` when
the last one closes.

## Change events

`{"method": "events.subscribe"}` → `{subscribed: true, revision, events}`. From then on the
connection also receives event lines (no `id`), at most ten bursts a second, coalesced:

```json
{"event": "project.changed", "revision": 42}
{"event": "selection.changed", "revision": 42, "layers": [3, 5]}
```

`revision` is the session's project revision (`app.revision` → `{revision}` reads it cheaply).
Replies and events share the connection: tell them apart by `event`. Closing the connection
unsubscribes.

## UI state

| Method | Params | Result |
|---|---|---|
| `ui.inspect` | `{}` | Window size, pixels per point, fps, serialized `UiState`, playback, time, active comp, selection, open dialog, last render ms. |
| `ui.elements` | `{prefix?}` | `[{id, label, rect: [x, y, w, h], window?}]`: the widgets drawn in the last frame, in points. On the desktop an undocked panel group is an OS window of its own: its widgets carry `window` (the group's id) and their rect is in that window's coordinates. |
| `ui.set` | `{tool?, workspace?, theme?, focused?, viewer?, timeline?, menuBar?, home?}` | `viewer: {zoom (null = fit), res: Full\|Half\|Third\|Quarter\|Auto, pan: [x,y], grid, rulers, safeMargins, transparencyGrid}`; `timeline: {pps (null = fit), start, graphEditor, showModes, openLayers: [ids], openGroups: [uids]}`. |
| `ui.panel.show` / `ui.panel.close` | `{panel}` | Panels: `Project`, `Composition`, `Timeline`, `EffectControls`, `EffectsPresets`, `Info`, `Preview`, `Character`, `Paragraph`, `Align` and others. |
| `engine.execute` `window.*` (panel menu) | `{panel, comp?, rect?, stacked?, solo?, small?, anchor?, zone?}` | The panel menu's items (≡ beside a tab name, or right-click a tab; automation ids `panelMenu.<key>`): `window.closePanel` (on a Timeline comp tab, `comp` closes that comp's Timeline), `window.floatPanel` (Undock Panel), `window.dockPanel` (with `anchor`/`zone`, or back to its usual place), `window.closeOtherPanels`, `window.closePanelGroup`, `window.floatPanelGroup`, `window.maximizePanel` (Maximize Panel Group, as `` ` ``), `window.stackPanelGroup`, `window.soloPanels` (Solo Panels in Stack: one expanded panel at a time; `solo`, or toggle), `window.smallTabs` (Small Tabs: a shorter tab strip; `small`, or toggle), and `window.panelMenu {panel, x?, y?}` to open the menu. |
| `ui.playback` | `{action: play\|stop\|toggle\|status}` | `{playing, time, audio, levelsDb, peaksDb}` (audio: an audio preview is running; L/R meter levels and held peaks in dBFS) |
| `ui.resize` | `{width, height}` | Resizes the window (points). |
| `ui.focus` | `{}` | Activates the window and takes keyboard focus. Agents normally avoid this, because the app is started without stealing focus. |
| `ui.figmaPair` | `{client}` | The one-time Figma pairing prompt the app's Figma bridge shows (**Connect Figma?**, automation ids `figmaPair.allow` / `figmaPair.deny`). The reply waits for the answer: `{allowed}` (a 120 s timeout counts as Deny). |
| `app.quit` | `{force?}` | Closes the app. A modified project shows the Save / Don't Save / Cancel prompt first (automation ids `dialog.unsaved.save`, `dialog.unsaved.dontSave`, `dialog.unsaved.cancel`) unless `force: true`. |

## Input

Points are logical window coordinates. A target is `{id}` (the element's centre; add `fx`/`fy` between
0 and 1 to pick a spot inside it) or `{x, y}`. `modifiers` is `{shift, alt, command, ctrl}`, where
`command` is ⌘ on macOS and Ctrl elsewhere.

| Method | Params |
|---|---|
| `ui.click` | `{id \| x,y, button?: left\|right\|middle, count?, modifiers?}` |
| `ui.move` | `{id \| x,y, down?, up?, button?, modifiers?}`: `down` presses and holds the button there, later moves drag, `up` releases |
| `ui.drag` | `{from: target, to: target, steps? (12), modifiers?}`: press, move, release |
| `ui.scroll` | `{id \| x,y, dx, dy}` |
| `ui.key` | `{key}`, for example `Space`, `F9`, `Cmd+Z`, `Cmd+Shift+Z`, `PageDown`, `Escape` |
| `ui.type` | `{text}`: text input into the focused field |

Input on an element in an undocked panel group's OS window (one with `window` in `ui.elements`)
goes to that window; with `x`,`y` pass `window` (the group id) for the same. `ui.key` and `ui.type`
go to the window the last pointer input went to.

Shortcut presets also accept `Physical+<shortcut>` for a keyboard position, using egui's
US-position names (for example `Physical+6` and `Physical+Minus` distinguish AZERTY's
`-` and `)` keys). Physical bindings take precedence over logical bindings for that key
press. `ui.key` accepts the same notation to test them. Ordinary shortcuts remain layout-aware.
`time.workAreaStart` and `time.workAreaEnd` navigate to the first and last included work-area
frames and can be assigned through `shortcuts.set`.

## Locating things

| Method | Params | Result |
|---|---|---|
| `ui.viewer.locate` | `{x, y}` (comp pixels) | `{x, y}` window point |
| `ui.viewer.hit` | `{id \| x,y}` (window point) | `{x, y}` comp pixel |
| `ui.timeline.locate` | `{layer (id), time?}` | `{x, y}` window point on the layer bar |

## Screenshots

| Method | Params | Result |
|---|---|---|
| `ui.screenshot` | `{path?, panel?, id?, max_side?}` | `{path, width, height}`: a PNG of the window, or cropped to one panel or element, downscaled so the longest side is at most `max_side` pixels (default 1600, 0 = full size). It captures the main window only: a panel in an undocked OS window is an error (dock it with `window.dockPanel` first). |

Element ids are stable, for example `tools.Selection`, `panel.Timeline`, `panel.tab.EffectControls`,
`panel.tab.Timeline.<comp>` (one Timeline tab per open comp; `panel.tab.Timeline` is the shown
one), `panel.tab.Viewer(<n>)` and `viewers.<n>` (View ▸ New Viewer's other Composition viewers;
`viewers.0` is the Composition panel while another viewer is active), `header.workspace.Animation`, `project.item.<id>` (a double-click opens the item, Enter
renames it), `viewer.comp`, `viewer.handle.<layer>.<i>`,
`timeline.layer.<id>.bar`, `timeline.layer.<id>.nestedMarker.<i>` (a precomp's comp markers), `timeline.layer.<id>.twirl`, `timeline.layer.<id>.row` (click selects,
Enter or a double-click renames, drag reorders), `timeline.prop.<uid>.stopwatch`,
`timeline.key.<uid>.<frame>`, `timeline.cti`, `effectControls.prop.<uid>.value` (an angle's
revolutions are `timeline.prop.<uid>.revolutions`, `effectControls.prop.<uid>.revolutions` and
`properties.prop.<uid>.revolutions`) and `effects.item.<name>`. The Composition viewer adds `viewer.magnification`, `viewer.resolution`,
`viewer.roi`, `viewer.grid`, `viewer.channel`, `viewer.exposure`, `viewer.snapshot`,
`viewer.showSnapshot`, `viewer.fastPreviews` (their popup entries are `viewer.<menu>Item.<n>`),
`viewer.ruler.top|left|origin`, `viewer.mask.<uid>.vertex.<i>`, `viewer.shapePath.<uid>.vertex.<i>`,
`viewer.motionPath.<layer>.<key>[.in|.out]`, `viewer.puppetPin.<uid>[.rotate|.scale]`,
`viewer.freeTransform.handle.<i>` and
`viewer.regionOfInterest`; the Graph Editor adds `timeline.graph.transformBox[.<i>]`,
`timeline.graph.snap` and `timeline.graph.reference`. Use `ui.elements` to see what is on screen.

Viewer state that agents drive headless too: `view.snapping`, `view.snappingOptions
{edgesExtended?, collapsedFeatures?, toggle?}` (the Tools bar's Snapping options,
`header.snappingOptions`: After Effects' Snap Edges Extended and Snap to Features in Collapsed
Compositions and Text Layers), `view.channel {channel, colorized?}`,
`view.exposure {stops | delta}`, `view.resetExposure`, `view.takeSnapshot`, `view.showSnapshot`,
`view.fastPreviewMode {mode}`, `view.setRegionOfInterest {rect}`, `view.addGuide`,
`view.moveGuide`, `view.removeGuide`; editing: `shape.newPath` (Pen on shape layers),
`shape.newShape` (the shape tools: a new group in the selected shape layer, else a new shape layer),
`shape.toolOptions` (the Tools bar's Tool Creates Shape / Mask and Fill and Stroke Options),
`mask.insertVertex`, `mask.convertVertex`, `mask.deleteVertices`, `path.freeTransform` (masks and
shape paths by uid), `keys.setSpatialTangents` (motion-path handles) and `keys.transform` (Graph
Editor transform box, timeline Alt-drag scaling).

The MCP server's bridge mode (`effectcraft-cli mcp --bridge 9877`) is a thin client of this protocol;
see [agents.md](agents.md).
