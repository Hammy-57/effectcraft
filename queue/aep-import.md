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

**Converter dependency: a question for the maintainers.** The converter is an optional external
Python tool, [py-aep](https://github.com/forticheprod/py-aep) (MIT), used only while converting, the
way ffmpeg is used only from outside as a test oracle: Python and py-aep are needed at import time
only, the resulting `.ecproj` needs neither, and EffectCraft itself stays pure Rust with no Python
code linked in. `docs/gaps.md` lists this route (#248) as an open clean-room question, so: is an
optional external converter like this acceptable to you? If not, the IR and the `effectcraft-aep`
crate stay useful for a future native reader. This PR changes no AGENTS.md rule.

**Measured fidelity.** On 9 October, on our fork, 255 SceneYard template projects
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
- `rustfmt --check` and `cargo check` of the affected crates.
- `cargo test`: `effectcraft-aep` 71/71; engine `tests_aep` 4/4 and `tests_menu_cmds` 46/46;
  keyframe and text pass.
- `cargo clippy --all-targets -- -D warnings`: clean for aep, engine, keyframe, text, project,
  automation, ui-egui and xtask.
- Not run: converting a real `.aep` end to end (needs Python, py-aep and After Effects projects) and
  the full `cargo xtask ci`.

Cherry-picked from our fork and adapted to upstream `main`: the importer crate as it
stands on the fork, plus the parts of `78b87fe6`, `b8bdb609`, `90b54de2`, `402136ca`, `c8c66ed1`,
`f5329b04`, `209c1364`, `0fa24be1`, `ba3ec8a2`, `de3aa77a` and the other AEP commits that it needs;
`Value::Path` / `Value::Gradient` stay unboxed as upstream has them.
