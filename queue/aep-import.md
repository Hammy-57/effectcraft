title: feat(aep): import After Effects projects through an external py-aep converter
branch: upstream/aep-import
depends: none

## What
`.aep` import (File ▸ Import ▸ After Effects Project..., `file.importAep`; `file.importAeIr` for an
already converted IR file):

- `packaging/aep/aep_ir_export.py` reads the `.aep` with [py-aep](https://github.com/forticheprod/py-aep)
  (MIT, pinned in `requirements.txt`) and writes a versioned JSON IR; `convert_aep.py` is a
  standalone converter.
- New L3 crate `effectcraft-aep` turns the IR into editable EffectCraft objects: comps, layers,
  keyframes with eases, masks, shapes (repeaters, dashes, gradients), text with styles, animators
  and path options, effects through an AE match-name map (`integrations/effectcraft/ae_effect_map.*`),
  layer styles, 3D layers / cameras / lights / models, Puppet meshes, Roto Brush controls, markers
  and Essential Properties. Anything it can't represent is reported as a fidelity loss; an import
  with losses is refused unless `allowLosses: true`.
- Data the importer needs: `TextDoc` / `CharStyle` keep the requested font's version, a verified
  linked font file (path + SHA-256) and a render substitute (`render_font`), `digit_set` and
  `auto_hyphenate`; gradients keep colour/opacity midpoints; `SourceLayer.crop_bounds`; Puppet
  `MeshGeometry`. Text resolves linked fonts and reports exact / substituted / missing fonts in
  `file.findMissing`.
- The engine runs the converter through `Services::export_aep_ir` (`EFFECTCRAFT_AEP_PYTHON`,
  `EFFECTCRAFT_AEP_EXPORTER`); browser hosts return Unsupported. Big imports run as a background task.
  macOS packaging bundles the exporter script. Confined MCP sessions check the paths it reads.

## Why
Opening existing After Effects work is gap 1 in `docs/gaps.md`.

**Converter dependency.** py-aep is used as an external converter at import time only, like ffmpeg
is used only from outside: Python and py-aep are needed while converting; the resulting `.ecproj`
needs neither, and EffectCraft itself stays pure Rust. Using py-aep this way is the clean-room
question raised in #248, so this PR needs the owner's decision on that first. (No AGENTS.md change
is included; the fork records its own decision there.)

**Measured fidelity (fork, `docs/gaps.md` G4).** On 9 October, 255 SceneYard template projects
converted and rendered close to After Effects' own renders: median mean-absolute error 0.018, with 2
projects clearly off (a liquid-glass wave shape, 3D lighting); `.aepx`, `.mogrt` and `.ffx` are not
read. That measurement includes the fork's AEP fidelity fixes to effects and the renderer (about 60
commits: Deep Glow, Radio Waves, CC Light Sweep, 4-Color Gradient, Orientation order, light
intensity, text indents, cropped Illustrator layers, ...). They are **not** in this PR: it is the
importer and the data it needs, so imported projects open and stay editable, but render less
faithfully until those follow. Six importer tests that check effect parameters added by those fixes
(Glow operation, CC Page Turn, CC Sphere, Curves, inert effect defaults, model material keys) are
left out with them.

## How tested
- `rustfmt --check` and `cargo check` of the affected crates (queue-pr.sh).
- `cargo test`'s `tests_aep` / `tests_menu_cmds`: aep 71/71, engine `tests_aep` 4/4 and `tests_menu_cmds` 46/46, keyframe and text pass; `cargo clippy -D warnings` clean for aep, engine, keyframe, text, project, automation, ui-egui and xtask.
- Not run: a conversion of a real `.aep` (needs Python + py-aep and After Effects projects), and
  the full `cargo xtask ci`.

Cherry-picked from eliasx45/effectcraft and adapted to upstream `main`: the importer crate as it
stands on the fork, plus the parts of `78b87fe6`, `b8bdb609`, `90b54de2`, `402136ca`, `c8c66ed1`,
`f5329b04`, `209c1364`, `0fa24be1`, `ba3ec8a2`, `de3aa77a` and the other AEP commits that it needs;
`Value::Path` / `Value::Gradient` stay unboxed as upstream has them.
