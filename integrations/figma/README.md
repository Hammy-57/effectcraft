# Figma → EffectCraft

Bring Figma layers into the open EffectCraft composition as **native, editable layers**:
shape layers with real paths and fills, text layers you can retype, precomps for frames, nulls for
groups, track mattes for masks, layer styles for shadows.

There are two ways in, both without a Figma token, and both use the same importer (the engine
command `file.importFigma`) and the same serializer ([`serialize.js`](serialize.js)):

- **Agents** get a Figma link and import it through the Figma MCP. Say "import <figma link>".
- **People** select layers in Figma and click **Send** in the EffectCraft Transfer plug-in.

## Agents: "import <figma link>"

Claude sessions with the Figma MCP signed in use the skill in [`skill/SKILL.md`](skill/SKILL.md)
(install it at `~/.claude/skills/figma-to-effectcraft/SKILL.md`). It:

1. parses the link (`figma.com/design/<fileKey>/…?node-id=12-345`);
2. runs `serialize.js` inside the file with the MCP's `use_figma` tool (Figma Plugin API, headless,
   no token). Its replies are capped at 20 KB, so `effectcraftSerialize({ids, fileKey, images:
   'none', budget: 18000})` sends what fits and lists the rest in `pending`; calls with
   `{subtree: [pending ids]}` return those subtrees as `fragments` until nothing is pending;
3. fetches every node listed in `rasterize` as a PDF with the MCP's `download_assets` (PNG only if
   the PDF fails), saving the URLs with `curl`;
4. writes each reply to a part file and runs
   `effectcraft-cli exec file.importFigma --bridge 9877 --params '{"payloadPaths": ["part-1.json", …]}'`;
5. reports what came over natively and what came in as images (and in which format).

**Optional: a token.** With a Figma personal access token, `file.importFigma` can fetch by itself:

```sh
export FIGMA_TOKEN=figd_…   # Figma ▸ Settings ▸ Security ▸ Personal access tokens (File content: read)
effectcraft-cli exec file.importFigma --bridge 9877 \
  --params '{"url":"https://www.figma.com/design/FILEKEY/Name?node-id=12-345"}'
```

- `--bridge 9877` runs it in the open app. Leave it out to run headless (add `--save-as out.ecproj`).
  Over MCP, call `execute_command` with `{"command":"file.importFigma","params":{…}}`.
- `url` is a link to the frame or layer (Figma: right-click ▸ Copy link to selection).
  `fileKey` + `nodeIds: ["12:345", …]` does the same.
- `target`: `newComp` (frame size → comp size), `activeComp` (centred in the open comp), or `auto`
  (the default: frames get new comps, anything else goes into the active comp).
- `payload` takes Figma node JSON directly: the REST `GET /v1/files/:key/nodes?geometry=paths` reply, the
  serializer's output, a node or an array of nodes. Use `payloadPath` for a JSON file, or
  `payloadPaths` for a payload in parts (the first file has `nodes`; later ones add `fragments`
  for `{"$ref": id}` stubs, and `images`). Images can be included as
  `"images": {"<node id>": {"pdf" | "png": "<base64>"} | {"pdfPath" | "path": "<file>", "bounds"?: "render"}}`;
  an entry may list a PDF and a PNG, and the PNG is used only if the PDF can't be imported.
- `dryRun: true` returns the plan (comps, layers, what would be rasterised) and changes nothing.
- The token comes from `token`, `FIGMA_TOKEN`, or one stored with
  `effectcraft-cli exec figma.setToken --params '{"token":"figd_…"}'` (kept beside the settings,
  readable only by you). It is never journaled, logged or saved in projects, and it is sent only to
  `api.figma.com`. `figma.status` says which token is in use.
- Network access uses the system's `curl` (macOS, Windows 10+ and most Linux systems ship it).

The reply lists the new comps and layers, how many came in natively, which nodes were rasterised and
why, and notes about anything approximated.

## People: the Figma plug-in

1. **Install once.** In the Figma desktop app: **Plugins ▸ Development ▸ Import plugin from
   manifest…** and pick `integrations/figma/plugin/manifest.json` from this repository.
2. **Open EffectCraft.** It runs the Figma bridge itself on `127.0.0.1:7789` (Settings ▸
   Scripting & Expressions ▸ **Let Figma Send Layers**, on by default). No terminal.
3. **Connect once.** Run **Plugins ▸ Development ▸ EffectCraft Transfer** and click **Connect to
   EffectCraft**. EffectCraft comes forward and asks **Connect Figma?**: click **Allow**. The
   plug-in keeps the pairing code (in Figma's client storage), so this happens only once.
4. **Send.** Select layers, choose **New comp** or **Into active comp**, then click **Send**. Each
   transfer is one undo step in EffectCraft.

The bridge listens on 127.0.0.1 only, accepts browser requests only from the plug-in (origin
`null`) or figma.com on the loopback host name, and imports only with the pairing code, which it
hands out only after you click Allow (one prompt at a time; after Deny it waits a few seconds).
Any sandboxed page shares the plug-in's `null` origin, so the code is the real protection: it is
12 characters from the operating system's secure random generator, and after 5 wrong codes in a
row the bridge refuses codes for a minute. The code is stored in the EffectCraft settings folder
(`figma-bridge-code`, created readable only by you). One transfer can be up to 64 MB (embedded
image renders count); send a very large selection in several parts.

**Without the app's bridge** (the setting off, or a headless setup), run
`effectcraft-cli figma-bridge` (`--port 7789`, `--control 9877`, `--code CODE`): it forwards to the
app's control channel and prints the pairing code, which you paste into the plug-in once (it shows
a code field instead of Connect).

To use another port, change it in `manifest.json` (`devAllowedDomains`) and in `ui.html`
(`BRIDGE`); `EFFECTCRAFT_FIGMA_BRIDGE_PORT` moves the app's bridge.

**Editing the plug-in:** `plugin/code.js` is generated. Edit `serialize.js` or `plugin/src/main.js`
and run `./build-plugin.sh` (a test fails when `code.js` is out of date).

## What transfers natively

| Figma | EffectCraft |
|---|---|
| Frame, component, instance, section (selected on its own) | A composition the size of the frame, with its fill as a background shape layer |
| Nested frame | Precomp. Clipping frames clip to their size, rounded corners become a layer mask. Opacity, blend mode, shadows and blur go on the precomp layer |
| Group | A null named after the group, with its children parented to it. A group with opacity, a blend mode, effects or a mask role becomes a precomp |
| Rectangle | Rectangle path (size, roundness). Different corner radii become an editable bezier path |
| Ellipse | Ellipse path. Arcs and donuts become a bezier path |
| Polygon, star | Bezier path from the geometry. A plug-in payload without geometry becomes a Polystar |
| Line, vector, boolean operation | Editable bezier paths. Boolean operations are flattened to their result path |
| Solid fills, linear and radial gradients | Fill and Gradient Fill (several fills stack in Figma's order) |
| Strokes | Stroke or Gradient Stroke: width, caps, joins, miter, dash and gap. Inside and outside alignment use an inset or outset copy of the shape (Offset Paths for free-form paths) |
| Text | Paragraph text: font family and style, size, colour, line height, letter spacing, alignment, upper case and small caps, paragraph spacing, and per-character style runs |
| Opacity, blend modes, hidden layers | Layer opacity, blend mode, video switch off |
| Drop shadow, inner shadow | Layer Styles ▸ Drop Shadow / Inner Shadow (colour, opacity, angle, distance, size, spread) |
| Layer blur | Gaussian Blur |
| Mask layers | Alpha or luma track mattes for the layers above the mask (the matte layer is hidden) |

Every layer's anchor point is at the centre of its Figma object, so scaling and rotating behave as
they do in Figma. Each layer's comment holds its Figma node id.

## What comes in as an image

These come in as **PDF** footage, placed exactly over the node. EffectCraft turns on Continuously
Rasterize for them, so they stay sharp at any scale, and the page has no background (transparent
where the node is). When a PDF can't be exported or imported, a PNG render at 2× (the `scale`
parameter) is used instead; the reply's `rasterized` entries say which `format` each node used.

- Layers with image, video, pattern or emoji fills (the whole layer, with its stroke and corners).
  A frame with an image fill comes in as one image, children included.
- Node types without an EffectCraft counterpart: widgets, embeds, stickies, connectors, tables, and
  so on.

Why PDF and not SVG: both stay sharp when scaled, but Figma's SVG export (tested through the Figma
MCP's `download_assets`) adds an opaque canvas-coloured rectangle behind the node, and EffectCraft's
SVG import drops what the SVG carries as filters and embedded images (drop shadows, image fills).
The PDF export imports with all of it and with a transparent background.

Renders come from Figma's image export (`exportAsync` in the plug-in and `use_figma`, `GET /v1/images` with a token, or the Figma MCP's `download_assets`). They are
saved next to a saved project (`<project> Figma Assets/`) or in the settings folder, and imported as
footage in a `<name> (Figma)` folder.

## Approximations and gaps

- Angular and diamond gradients come in as radial gradients. Elliptical radial gradients become circular.
- Background blur is skipped. Only the first drop shadow and the first inner shadow are kept (one of
  each per layer style). Shadow spread maps to a percentage.
- Underline and strikethrough aren't supported. Lowercase and title case change the text itself.
- A text layer whose box was auto-width gets a little extra width, so different font metrics don't
  wrap it. The vertical position follows Figma's line height. Fonts that aren't installed fall back
  to Inter, which is bundled.
- A vector that only has `strokeGeometry` (REST without `fillGeometry`) is rasterised. Ask for
  `geometry=paths`, which `file.importFigma` always does.
- Skewed transforms are placed without the skew (noted). The root frame's own opacity and effects
  stay in Figma when it becomes the composition.
- Auto-layout, constraints, components and variables aren't kept. Each node arrives as it looks.
