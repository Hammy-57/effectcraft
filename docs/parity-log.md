
## Update: File ▸ Import ▸ EffectCraft Project

- **Import a project into a project**, as After Effects imports an `.aep`:
  `file.importProject {path}` (File ▸ Import ▸ EffectCraft Project..., and `.ecproj` /
  `.ecprojx` files given to File ▸ Import) merges the project into a folder named after the
  file. Every item, layer and property id moves past the open project's, so references
  (precomps, parents, track mattes, Layer Control values, Essential Graphics, tracker
  targets) stay intact. It's one undo step. Footage paths relative to the imported project
  resolve next to it. The result lists the comps, the root comps, `primaryComp` (the only
  root comp) and missing media.
- Id shifting saturates instead of overflowing on damaged files, for both templates and
  projects.
