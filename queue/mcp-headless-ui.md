title: feat(mcp): headless UI for MCP (mcp --ui)
branch: upstream/mcp-headless-ui
depends: upstream/mcp-protocol

## What
`effectcraft-cli mcp --ui [--ui-size 1680x1020] [--ui-scale 2]` runs the whole app offscreen (egui_kittest + wgpu, no window) inside the MCP server, so `screenshot` (with panel/id crops), the `ui_*` tools and `control` work without a running app. Every tool goes through the same control-channel requests as bridge mode; clicks, drags, keys and typing are real input events. `crates/ui-egui` gets an `offscreen` module, the backend a `ControlTransport` abstraction (the bridge is one implementation).

## Why
Bridge mode needs a desktop window on screen and focus. Agents (and CI machines) can't always have one, and several agents need to run side by side without stealing each other's focus.

## How tested
- `cargo test -p effectcraft-automation --lib` (53 passed; includes a fake `ControlTransport` driving the UI tools)
- `cargo check -p effectcraft-cli -p effectcraft-ui-egui --tests`
Not run: `apps/effectcraft-cli/tests/mcp_ui.rs` (needs a GPU adapter; this machine has none). No `cargo xtask ci`.

Depends on the MCP protocol PR (`upstream/mcp-protocol`); open this one after it is merged.

Split out of #859 at the maintainer's request on #858.
