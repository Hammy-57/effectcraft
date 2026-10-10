---
name: figma-to-effectcraft
description: Import Figma frames or layers into the running EffectCraft app as native, editable layers (shape layers, text, precomps, layer styles), with no Figma token and no terminal setup. Use when the user gives a figma.com link and says import / bring / send / transfer it into EffectCraft (or "into my comp", "as layers", "animate this Figma frame"). Reads Figma through the Figma MCP's use_figma tool and imports with file.importFigma.
---

# Figma → EffectCraft (no token)

The Figma MCP is already signed in, and `use_figma` runs Plugin API code inside the file. The
shared serializer turns nodes into the payload EffectCraft's `file.importFigma` reads. No
`FIGMA_TOKEN`, no plug-in, no Figma desktop app. Reading never changes the Figma file.

Paths (EffectCraft checkout `EC=/Users/elias/Developer/effectcraft`):
- serializer: `$EC/integrations/figma/serialize.js`
- CLI: `$EC/target/release/effectcraft-cli` (else `cargo run --release -p effectcraft-cli --` in `$EC`)
- control port: **9877** (Elias's app; Settings ▸ Let Agents Control EffectCraft). Use another
  port only when the user names one. Never quit or reset the user's app.
- work folder: `W=/tmp/effectcraft-figma/<fileKey>-<nodeid>/` (one per import)

`use_figma` replies are capped at 20 KB, so a frame usually arrives in several **parts**: the
first holds `nodes`, later ones hold `fragments` that fill in `{"$ref": id}` stubs. EffectCraft
reassembles them (`payloadPaths`). Images never travel through the conversation: they come
from `download_assets` URLs, fetched with `curl`.

## Steps

1. **Parse the link.** `https://www.figma.com/design/<fileKey>/<name>?node-id=12-345` gives
   `fileKey` and node id `12:345` (`-` becomes `:`). For `/branch/<branchKey>/` links, use the
   branchKey. No `node-id`: run `get_metadata {fileKey}` and ask which frame, or take the frame
   the user named. Several ids are fine.
2. **Serialize the structure** (load the `figma-use` skill first; pass `skillNames: "figma-use"`).
   Call `use_figma` with `fileKey` and as `code` the full text of `serialize.js` (Read it; send
   it unchanged) followed by:
   ```js
   return await effectcraftSerialize({ ids: ['12:345'], fileKey: '<fileKey>', images: 'none', budget: 18000 });
   ```
   Write the reply, exactly as returned, to `$W/part-1.json` (Write tool; don't re-format).
3. **Fetch the rest.** While the last reply's `pending` is not empty, call `use_figma` again
   with `serialize.js` +
   ```js
   return await effectcraftSerialize({ subtree: [<the pending ids>], fileKey: '<fileKey>', images: 'none', budget: 18000 });
   ```
   and write each reply to the next `$W/part-N.json`. Every reply says what is still `pending`.
4. **Images.** Collect the ids in every part's `rasterize` (nodes with image fills, or types
   with no native counterpart, or `tooLarge` subtrees). For each id:
   - `download_assets {fileKey, nodeId: '<id>', defaultFormat: 'pdf'}` then
     `curl -sfL -o $W/<id>.pdf '<export.url>'` (`:` → `-` in file names).
   - Write `$W/images.json`:
     `{"images": {"<id>": {"pdfPath": "$W/<id>.pdf", "bounds": "render"}, …}}`.
     PDF is the default: it stays sharp when scaled (EffectCraft turns on Continuously
     Rasterize) and keeps transparency. `bounds: "render"` because that export covers the
     node's render bounds (shadows included).
   - PNG only as the fallback: if a PDF download fails, or the import note says
     "PNG used" / "image not imported", fetch `download_assets {…, defaultFormat: 'png',
     defaultScale: 2}`, curl it to `$W/<id>.png` and add `"path": "$W/<id>.png"` next to (or
     instead of) `pdfPath`. EffectCraft tries the PDF first, then the PNG.
   - Instance sub-layer ids (`I12:3;4:5`) are refused by `download_assets`: render those
     with `use_figma` + `serialize.js` + `return await effectcraftSerialize({ imagesOnly: ['<id>'], images: 'png', scale: 1 });`
     (keep it under 20 KB; skip if it isn't) and write the reply as a part.
5. **Import into the open app:**
   ```sh
   "$EC/target/release/effectcraft-cli" exec file.importFigma --bridge 9877 --json \
     --params '{"payloadPaths": ["$W/part-1.json", "$W/part-2.json", …, "$W/images.json"]}'
   ```
   (`part-1.json` first.) Options in `--params`: `"target": "newComp" | "activeComp" | "auto"`
   (default auto: frames become new comps, other layers go into the active comp), `frameRate`,
   `duration` (s), `"dryRun": true` (plan only). It is one undo step.
   - "the payload part for node … is missing": a `pending` id wasn't fetched; do step 3 for it.
   - `connection refused` / `cannot connect`: EffectCraft isn't running or agent control is off.
     Ask the user to open EffectCraft (Settings ▸ Scripting & Expressions ▸ Let Agents Control
     EffectCraft).
   - `unknown command file.importFigma` or "payloadPaths" not understood: the app is older than
     this skill; ask to update.
   - `target activeComp: no composition is open`: use `newComp`.
6. **Report** from the reply: the comp(s) created, `native` layers vs `images`, each
   `rasterized` node with its reason and `format` (pdf / png), and `notes` (approximations).
   Keep it short.

Small selections (one reply under 20 KB, no `pending`) skip step 3; selections without
`rasterize` entries skip step 4.

## What comes over

Native: frames (comps / precomps, clipping, rounded corners), groups (nulls), rectangles,
ellipses, polygons, stars, vectors, booleans (paths), solid and linear / radial gradient fills,
strokes, text with style runs, opacity, blend modes, drop / inner shadows, layer blur, masks
(track mattes). As PDF footage (PNG if the PDF fails): image / video fills, and node types
without a counterpart. Details: `$EC/integrations/figma/README.md`.

A Figma token (`FIGMA_TOKEN`, `figma.setToken`) is an optional extra: with one,
`file.importFigma {"url": "<link>"}` fetches by itself. Don't ask for one.
