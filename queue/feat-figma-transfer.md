title: feat(integrations): Figma → EffectCraft import (file.importFigma) and transfer plug-in
branch: upstream/feat-figma-transfer
depends: none

## What
`file.importFigma` brings Figma frames and layers in as native, editable layers:
- comps and precomps, group nulls, shape layers with editable paths, fills, gradients and strokes;
- text with style runs, track mattes, layer styles and Gaussian Blur;
- image fills and unsupported nodes as 2× PNG renders.

The source is a Figma link through the REST API (system `curl`; the token comes from `FIGMA_TOKEN` / `figma.setToken` and is never journaled), or node JSON passed in. Each import is one undo step.

`effectcraft-cli figma-bridge` is a local HTTP endpoint for the Figma plug-in in `integrations/figma/` (README, `manifest.json`, `code.js`, `ui.html`).

## Why
Designers hand off in Figma. This turns a frame into editable EffectCraft layers instead of a flat image.

## How tested
- `crates/engine/src/figma/tests.rs`: REST and plug-in fixture JSON mapped to layers.
- `cargo clippy -p effectcraft-engine -p effectcraft-cli --all-targets -- -D warnings` and rustfmt pass on this branch.
- A later fork change (the app-hosted bridge with one-time pairing) isn't included. It builds on other unshared work.
