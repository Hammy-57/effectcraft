title: feat(control): stable layer ids, per-client undo, control-channel events and retries, agent-friendly errors
branch: upstream/control-channel
depends: upstream/mcp-headless-ui

## What
Agents sharing one running app no longer step on each other, and failures say what to do:
- **Stable layer ids.** A number in a layer/effect/mask/tracker/animator reference is an id only; the silent fallback to a 1-based stack position is gone (`"#n"` stays positional; unknown ids error with that hint). Ids are never reused across undo/redo.
- **Per-client undo and document revision.** `expectRevision` refuses to run on a stale document; `client` tags undo steps and `edit.undo {client}` undoes only that client's last step. `project.summary` / `editor.state` carry the revision. Layer Style Cancel restores only its own layer when others edited meanwhile.
- **Control channel.** A request that times out before the UI thread starts it is cancelled and never runs (`notRun: true`); `requestId` makes retries idempotent (128-entry reply cache); `events.subscribe` streams `project.changed` / `selection.changed`; a busy port is an error the user sees; `EFFECTCRAFT_CONTROL_PORT`; `ui.screenshot {max_side}`; `app.revision`.
- **Agent-friendly errors.** Clamped values come back as `warnings`; bad paths, popup values, commands, effects and tool args list the valid choices or the closest match; typed index/enum schemas; `brief` discovery (`command_list`, `list_effects`); mutations return what changed.
- A property-group path error names the "is a group, not a property" hint with its children.
- `mcp --ui --autosave` and the `snapshot` example reuse the offscreen UI; `ui.move` takes `down`/`up`/`button` for step-by-step drags.

## Why
With several agents (and a person) on one document, stack positions and "last undo" silently hit the wrong object, a retried request could run twice, and a timed-out one could still run later. The error messages were written for people who can see the screen.

## How tested
- `cargo test -p effectcraft-engine --lib` (563 passed, 2 ignored; the id change touched many engine tests)
- `cargo test -p effectcraft-automation --lib` (55 passed)
- `cargo test -p effectcraft-ui-egui --test ui_control_channel` (3 passed)
- `cargo check -p effectcraft-cli -p effectcraft-ui-egui -p effectcraft -p effectcraft-automation --tests`
Not run: the other `ui-egui` suites, the CLI integration tests, `mcp_ui` (needs a GPU adapter). No `cargo xtask ci`.

Depends on the headless UI PR (`upstream/mcp-headless-ui`, which depends on `upstream/mcp-protocol`); open this one after those are merged.

Split out of #859 at the maintainer's request on #858.
