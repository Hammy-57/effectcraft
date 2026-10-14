# Paste from FigCraft

In FigCraft, select artwork and use ordinary **right-click → Copy**, **Edit → Copy**, or
**Cmd+C / Ctrl+C**. In EffectCraft, use **Edit → Paste** or **Cmd+V / Ctrl+V**.
The explicit **Copy for EffectCraft** action remains optional. Frames create new compositions; other selections go into
the active composition, or create one when none is open. Each paste is an independent copy and
one undo step. Later edits in either application do not change the other copy.

Shapes, paths, fills, strokes, text, groups and nested frames become native editable layers.
The existing native design mapper also handles supported masks, shadows and layer blur.
Unsupported artwork is omitted with warnings; this route never turns it into a screenshot or
imports external files. Image/pattern paints, advanced gradients, text metrics and effects can
differ; the paste reports the exporter and mapper warnings. Exact pixel parity has not been
established. Pasting while editing text keeps the ordinary text-paste behavior.

## Agent command

`edit.pasteFigCraft {clipboard, target?: auto|newComp|activeComp, comp?}` takes the system
clipboard text: the SVG from ordinary Copy, or the explicit action's JSON. `auto` follows the behavior above. New compositions use 30 fps and a
10-second duration; pasting into an existing composition uses its frame rate and duration, including
nested compositions. The result includes `comp`, `layers`, `native`, `warnings` and `notes`.

The envelope is `{format: "figcraft.clipboard", version: 1, nodes: [...], warnings: [...]}`.
Nodes use the public Figma-shaped geometry fields understood by the existing mapper; this is a
FigCraft transfer format, not Figma's clipboard. Ordinary Copy embeds its standard padded-base64
encoding in `<metadata id="figcraft-clipboard-v1">…</metadata>` inside SVG; other applications
can still use the SVG. EffectCraft reads only its exact metadata marker and leaves ordinary SVG
paste unchanged. Duplicate, malformed or oversized marked payloads return an error without
falling back to SVG or anything previously copied inside EffectCraft.

The SVG carrier is capped at 8 MiB; the decoded JSON remains capped at 2 MiB, 20,000 nodes and
64 node nesting levels. JSON nesting is independently bounded before parsing. SVG path data is
accepted in `fillGeometry` and `strokeGeometry` entries; external asset paths, image payloads,
fragment references and URL fetch inputs are rejected.


## Verification (14 October 2026)

Actual FigCraft right-click Copy and Cmd+C through the system clipboard, followed by EffectCraft
menu Paste and Cmd+V, produced ten native layers (three text, seven shapes, including an editable
vector) without images. Cut/Undo, plain-text copying and the optional explicit action also passed.
Native checks covered text editing, one-step paste undo/redo and viewer restoration, independent
copies, malformed-carrier rejection without mutation, identical pixels after save/reopen, and
standalone line/shadow bounds (220 × 120 in both applications). Earlier explicit-transfer checks
also covered nested editable text and existing-comp timing (24 fps / 20 seconds).

A synthetic linear-gradient region differs by at most 1/255 per channel between the apps;
text metrics still differ and are warned. These checks do not establish general pixel parity.
Scoped engine/UI compilation and clippy with all targets and warnings denied passed, as did the
native app build. Regression tests were compiled but not executed under the fork's daytime
rule; full CI was not run.
