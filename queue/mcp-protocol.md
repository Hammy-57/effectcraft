title: feat(mcp): structured results, resource templates, prompts, notifications, progress and cancel, get_layout, analyze_audio
branch: upstream/mcp-protocol
depends: none

## What
The MCP server speaks more of the protocol, and agents get three more analysis tools:
- **Structured results** (`structuredContent` + `outputSchema`) for clients that negotiated 2025-06-18 or later; the text block stays for older ones.
- **Resource templates** (`effectcraft://comp/{comp}`, `layer/{comp}/{layer}`, `frame/{comp}/{time}` as a PNG blob, `command/{id}`) and **prompts** (`title_card`, `lower_third`, `port_ae_script`, `check_against_reference`, `animate_layer`).
- **Notifications**: `resources/subscribe` (updates after any change to the project revision) and `logging` (expression errors, missing footage, failed auto-saves, toasts).
- **Progress and cancellation** for long jobs (render queue, tracking and analyses, `jobs.wait`, footage check) and for headless `run_script` (`notifications/progress`, `notifications/cancelled`, a script timeout that rolls back an open undo group).
- **Duplicate tool aliases hidden** from `tools/list` (`list_commands`, `execute_command`, `get_project`, `render_frame` stay callable; `mcp --all-tools` lists them). `render_preview` gains `path` and `inline`.
- **`get_layout`** (`layer.bounds`): per layer whether it is drawn and why not, comp-space bounds, corners, centre, anchor, on-screen share, optional drawn-pixel bounds and pairwise overlaps.
- **`analyze_audio`** (`audio.analyze`): peak, true peak, RMS, integrated/momentary/short-term loudness (BS.1770-4), clipping, silences, optional waveform image.

## Why
Agents drive EffectCraft over MCP and need to know what changed, wait on long work and cancel it, find layers on screen, and check audio without a render. Newer MCP clients also expect structured output, resource templates and prompts, and a long render used to block the connection with no progress.

## How tested
- `cargo test -p effectcraft-automation --lib` (52 passed)
- `cargo test -p effectcraft-script --lib` (80 passed: the script interrupt)
- `cargo test -p effectcraft-engine --lib -- layout audio_analysis jobs` (12 passed)
- `cargo check` of automation, cli, engine and script with `--tests`.
Not run: the CLI integration tests and the full engine/ui suites. No `cargo xtask ci`.

Split out of #859 at the maintainer's request on #858.
