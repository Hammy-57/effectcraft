# Paste from FigCraft

In FigCraft, select artwork and choose **Edit → Copy for EffectCraft**. In EffectCraft, use
**Edit → Paste** or **Cmd+V / Ctrl+V**. Frames create new compositions; other selections go into
the active composition, or create one when none is open. Each paste is an independent copy and
one undo step. Later edits in either application do not change the other copy.

Shapes, paths, fills, strokes, text, groups and nested frames become native editable layers.
The existing native design mapper also handles supported masks, shadows and layer blur.
Unsupported artwork is omitted with warnings; this route never turns it into a screenshot or
imports external files. Image/pattern paints, advanced gradients, text metrics and effects can
differ; the paste reports the exporter and mapper warnings. Exact pixel parity has not been
established. Pasting while editing text keeps the ordinary text-paste behavior.

## Agent command

`edit.pasteFigCraft {clipboard, target?: auto|newComp|activeComp, comp?}` takes the same JSON text
as the system clipboard. `auto` follows the behavior above. New compositions use 30 fps and a
10-second duration; pasting into an existing composition uses its frame rate and duration, including
nested compositions. The result includes `comp`, `layers`, `native`, `warnings` and `notes`.

The envelope is `{format: "figcraft.clipboard", version: 1, nodes: [...], warnings: [...]}`.
Nodes use the public Figma-shaped geometry fields understood by the existing mapper; this is a
FigCraft transfer format, not Figma's clipboard. It is capped at 2 MiB, 20,000 nodes and 64 node
nesting levels. JSON nesting is independently bounded before parsing. File paths, image payloads,
fragment references and URL fetch inputs are rejected. An invalid marked clipboard returns an
error without pasting anything previously copied inside EffectCraft.


## Verification (14 October 2026)

Actual FigCraft UI Copy through the system clipboard, followed by EffectCraft menu Paste and
Cmd+V, produced nine native layers (three text, six shapes) without images. Native checks covered
text editing, one-step paste undo/redo and viewer restoration, independent copies, invalid
clipboard rejection without mutation, identical pixels after save/reopen, nested editable text,
existing-comp timing (24 fps / 20 seconds), and standalone line/shadow bounds.

A synthetic linear-gradient region differs by at most 1/255 per channel between the apps;
text metrics still differ and are warned. These checks do not establish general pixel parity.
Scoped engine/UI compilation and clippy with all targets and warnings denied passed, as did the
native app build. Regression tests were compiled but not executed under the fork's daytime
rule; full CI was not run.
