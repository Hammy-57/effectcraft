# After Effects project import

`file.importAep` runs the bundled `aep_ir_export.py` through Python, then imports its
versioned JSON into native EffectCraft project objects. The resulting `.ecproj`
does not require After Effects or Python to open or edit. The importer requires
Python with [`py-aep`](https://github.com/forticheprod/py-aep) installed while
converting; the SceneYard acceptance corpus uses py-aep commit
`e12a451c35bacd3f34a080090265f9370e66162b` (MIT).

Set `EFFECTCRAFT_AEP_PYTHON` to the Python interpreter that has py-aep installed.
The pinned dependency is in `requirements.txt` for a separate import-time venv.
The app looks for `aep_ir_export.py` in its macOS Resources folder or this
checkout's `packaging/aep/` directory. `EFFECTCRAFT_AEP_EXPORTER` overrides
that path. The exporter is also usable directly:

```sh
EFFECTCRAFT_CLI=/path/to/effectcraft-cli python3 packaging/aep/convert_aep.py source.aep source.ecproj

# Lower-level data inspection:
python3 packaging/aep/aep_ir_export.py source.aep -o source.aep.json
effectcraft-cli exec file.importAeIr --params '{"path":"source.aep.json"}' --empty --save-as source.ecproj
```

Imports reject any reported fidelity loss by default. Use `allowLosses: true`
only to inspect an incomplete import while implementing a missing native feature.
Missing footage remains an editable offline item with its original path so it
can be relinked later.
