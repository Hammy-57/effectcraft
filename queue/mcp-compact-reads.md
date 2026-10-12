title: feat(mcp): compact, paged read tools for big comps
branch: upstream/mcp-compact-reads
depends: none

## What
The MCP read tools answer compactly and page long lists:
- `get_comp` / `doc_inspect`: `layerCount` + compact layer rows (`index, id, name, type, in, out` plus only non-defaults: `start`, `stretch`, `parent`, `trackMatte`, `blendMode`, switch tags such as `hidden`, `3d`, `solo`, `quality:Draft`), paged with `offset`/`limit` (default 200), `returned`, `next_offset`.
- `get_property`: compact keyframes (interpolation omitted when Linear, values rounded to 4 decimals), paged (default 100, `keyCount`).
- `get_layer {flat: true}`: paged (default 150, `total`); a big tree carries a `hint`.
- A cut list says so in-band: `truncated: {returned, total, next_offset, hint}`.
- `full: true` keeps the old long shape (engine `comp.info` / `prop.get` are unchanged).

## Why
`get_comp` on a 110-layer comp was ~47–62k characters because each layer repeated its default switches, blend mode, stretch and label. Clients that cap a tool result at 8k characters saw 5 of 110 layers and took that for the whole comp. Compact: ~100 characters a layer (110 layers: 10.8k vs 47.3k).

## How tested
`cargo test -p effectcraft-automation --lib` (91 passed, 3 new: compact rows / omitted defaults / paging + next_offset / full unchanged / keyframe and flat paging) and `--test qa` (8 passed); `cargo clippy -p effectcraft-automation --all-targets -D warnings`.
