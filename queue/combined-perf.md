title: perf: faster effects, expressions, text, caches, movie reading, project open, export and UI playback
branch: upstream/combined-perf
depends: none

## What
**Effects**
- Four CPU kernels, bit-identical output: `Plane::transpose` is cache-blocked in 32×32 tiles (twice
  per vertical pass of `box_plane` / `gauss_plane`); Median updates its histogram inline through a
  generic column visitor; Camera Lens Blur sums iris span counts once per iris, not per pixel; Cell
  Pattern computes each cell's feature point once per frame (about 7× faster at High Quality).

**3D**
- Depth of field: the iris kernels gather only the two blur levels around each pixel's radius from
  one set of row prefix sums, instead of keeping up to eight full-image blurs (about 4× faster).
- Both 3D compositors skip preparing shadow casters when shadows are off (Draft without Realtime
  Shadows).

**Layer cache**
- Under budget pressure, quantised 8 and 16 bpc entries are packed as `u8` / `u16` (`PackedBuf`)
  instead of evicted; packing checks every value round-trips bit for bit and declines otherwise. The
  cache holds 2–4× as many 8/16 bpc layers at the same budget.

- More caches follow the Memory & CPU budget: the GPU upload cache drops freed layers' textures on
  every upload and evicts least recently used when over budget, with the texture pool sized from the
  layer-cache budget; Roto Brush segmentations get half the layer-cache budget (at most 768 MB); the
  disk-cache write queue is bounded by bytes.

**Media**
- Movies open through a file reader: the sample index is read at open and samples on demand; audio
  and movie probes read headers instead of decoding the whole file. Standalone audio is probed with
  `symphonia` (MPL-2.0, already in `Cargo.lock` through FilmCraft), now a direct dependency of
  `effectcraft-media`.

**Expressions**
- A run collects every missing host value at once and prefetches what the previous evaluation read;
  keyframe lists are read lazily; `sourceRectAtTime` on shape layers is geometric; a layer's own path
  is memoised.
- A per-thread memo shares results between evaluations; it returns a stored result only when every
  recorded host answer is bit-identical, never stores `Math.random` / `Date` scripts, and is bounded
  by bytes. A 100-layer expression rig drops from 23.4 to 14.7 ms per frame.

**Text**
- Glyph outlines are read under a `RwLock` and transformed straight from the shared `Arc`;
  `layout_doc` caches its finished `TextLayout`; the layout and shaping caches are byte-bounded LRUs
  (64 / 32 MiB); a `.ttc` file is read once for all of its faces; the UI shares the engine's font bytes.

**Project open**
- Property-tree nodes stream during loading (a hand-written `Deserialize` reads the `"node"` tag
  first instead of buffering every subtree); `migrate::is_current` avoids deep-cloning every effect
  to compare it with its upgrade. A 20.9 MB layer array parses in 72 ms instead of 226 ms.

**Export**
- Export follows the Memory & CPU budget: `JobOptions::memory_budget` (default 1 GiB) sizes the
  layer cache and bounds the frames in flight (one per core, 2–16, fewer when they'd exceed it).
- Movie and WebM exports render the next batch while the current one encodes and muxes (at most two
  batches held); serial on wasm and rayon workers.
- A Render Queue item with several output modules renders each frame once: modules that render the
  same frames share one render, each with its own encoder and muxer thread, and files stay
  byte-identical; others reuse the previous module's layer cache when the frames match.

**UI**
- Playback: viewers upload shared frames without copying pixels, passive viewers share the comp
  instead of cloning it every frame, a playing preview repaints when its next frame is due (not at
  the display rate), and the channel / exposure transform runs in parallel.
- Panels: thumbnail textures are bounded by an LRU; the Timeline cache bar remembers its frame lists
  per cache generation instead of scanning the caches under locks every UI frame; Lumetri Scopes
  share their cached scope; the Footage panel decodes on a worker thread at panel size; the native
  menu catches up after a drag and updates at most 4 times a second.

## Why
These were the hottest paths in effect renders, expression rigs, text layers, project open and
exports and the UI thread: rendering and encoding alternated, opening a long movie read the whole
file, playback copied every frame and repainted at up to 120 Hz, multi-module items rendered every frame N times, and a
slider read by N layers ran N times per frame.

## How tested
- `rustfmt --edition 2024 --check` on every changed `.rs` file: clean.
- `cargo clippy --all-targets -- -D warnings` for effects, engine, export, expr, gpu, host, media,
  project, render, text, ui-egui and effectcraft: clean.
- `cargo check --target wasm32-unknown-unknown` for effects, engine, export, expr, gpu, media,
  project, render and text: clean.
- `cargo test` for the same twelve packages: 2,055 passed (the perf changes keep output
  bit-identical where their tests say so: kernels, packed cache, export files).
- Known failures, identical on `main` on this runner: `theme::script_font_tests::arabic_words_are_joined_and_read_right_to_left`,
  and in `effectcraft-gpu`'s lib tests on llvmpipe two CPU/GPU mismatches (CC HexTile, CC Threshold RGB)
  and a segfault later in the suite.
- Not run: the full `cargo xtask ci`. Tests ran on a 4-core Linux runner without a GPU (wgpu on
  llvmpipe).

Cherry-picked with `-x` trailers from topic branches already adapted to current `main`.
