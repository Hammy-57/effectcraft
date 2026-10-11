title: feat(settings): Let Agents Control EffectCraft starts the control channel on every launch
branch: upstream/fix-settings-agent-control
depends: none

## What
New setting **Settings ▸ Scripting & Expressions ▸ Agents ▸ Let Agents Control EffectCraft** (`scripting.agentControl`, default off) with **Control Channel Port** (`scripting.agentControlPort`, default 9877). When on, the app starts the localhost JSON-lines control channel on every launch, as `--control <port>` does. `--control` on the command line still wins.

## Why
Using agents/MCP `--bridge` with the desktop app meant launching it with `--control` every time. Unlike `--control` (made for agent-launched instances), the setting keeps the normal launch behaviour: the window takes focus, and the Home screen and crash recovery behave as usual.

## How tested
- On this branch (upstream/main + this commit): `cargo clippy -p effectcraft-engine -p effectcraft --all-targets -- -D warnings`; `cargo test -p effectcraft-engine prefs`; `cargo test -p effectcraft-ui-egui --lib -- i18n prefs settings` (28 passed, including the catalog/settings-registry checks); rustfmt --check.
