title: perf(export): render each frame once for a Render Queue item's output modules
branch: upstream/perf-export-shared-frames
depends: upstream/perf-export-pipelined

## What
A Render Queue item with several output modules renders each frame once.
- `Exporter::export_modules_async` takes all of an item's output modules. The default exports them one after another, as before. The item's progress shows the module furthest behind.
- `export_modules`/`export_modules_async` export together the modules that render the same frames (same project, comp, Render Settings including the time span, session objects, memory budget, Crop and Resize). They use one render of each frame, and each module has its own encoder and muxer on its own thread. The other modules export alone. Batches keep the per-module partition, so files stay byte-identical.
- A module that can't share frames still continues from the previous module's overridden project and layer cache. The cache is kept in one slot, dropped after 3 s or when the next job takes it. It is reused only for the same frames: equal project, same comp, Render Settings equal except for the time span, same Switches Affect Nested Comps, budget, footage source, expression host and accelerator.
- The host's `FileExporter` overrides `export_modules_async`, so the app gets this.

## Why
Each module used to export as a fresh job, with a project clone and an empty layer cache, so every frame rendered N times for N modules.

## How tested
- `tests/output_modules.rs`: H.264, PNG sequence and ProRes files are byte-identical to each module exported alone.
- Host tests: byte identity, one footage request per frame for 2 and 3 modules, a stop mid-way and a failing module.
- On this branch: `cargo clippy --all-targets -- -D warnings` and `cargo test` pass for `effectcraft-export`, `effectcraft-engine` and `effectcraft-host` (rustfmt clean).

Depends on the pipelined export PR (`upstream/perf-export-pipelined`).
