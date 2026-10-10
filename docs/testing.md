# Testing

```sh
cargo test --workspace     # everything
cargo xtask ci             # fmt, clippy -D warnings, tests, layers, assets, wasm
cargo xtask ci --affected  # the same gates, only for the crates your change can affect
```

`xtask ci` builds clippy and tests with `--profile ci` (release optimisation without LTO, 16 codegen
units, output in `target/ci/`): fast to build, still optimised enough for the pixel and perf tests.

## Affected-only CI

The full `cargo xtask ci` builds and tests every crate with `--profile ci`.
For each change, agents and contributors run the affected-only variant; the full CI runs before a
release (and on any change it falls back to, below).

```sh
cargo xtask ci --affected                  # base: the merge-base of HEAD with main
cargo xtask ci --affected --base HEAD~3    # any git revision
cargo xtask ci --affected --dry-run        # print the plan only
```

It collects every file that differs from the base (committed, staged, unstaged and untracked) and
maps each one to:

- the workspace package whose directory holds it, and
- every package whose sources point at it with a `../` path, such as
  `include_bytes!("../../../assets/fonts/Inter-Regular.ttf")` or
  `concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/effects.md")`. So editing
  `docs/preferences.md` reruns the engine tests that check it.

The affected set is those packages plus everything that depends on them (transitively through
normal and build dependencies) and every package that uses one of them as a dev-dependency. For
that set it runs `cargo fmt --check` (whole workspace, it is fast), `cargo clippy --all-targets
--profile ci -- -D warnings`, the tests (`cargo nextest run --profile ci` plus `cargo test --profile ci
--doc` when cargo-nextest is installed, otherwise `cargo test --profile ci`), the layers check, the
assets check only when an asset, an `.attribution` sidecar or `ATTRIBUTION.md` changed, and the
wasm check only for affected L0–L4 crates and the web crates. It prints the plan first and a timing
table at the end:

```text
ci --affected: base bc0570888 (HEAD), 1 changed file(s)
  changed: automation
  affected: automation, cli (2 of 35 crates)
  steps: fmt, clippy (2), nextest (2), doctests (1), layers (assets skipped: no asset changes)
```

Changes to the root `Cargo.toml`, `Cargo.lock`, `.cargo/`, `xtask/`, `rust-toolchain(.toml)` or
`clippy.toml` can change every crate's build, so they run the full CI instead and say why. Changes
no crate reads (most of `docs/`, `README.md`, `.github/`, `packaging/`) only get the cheap checks.

Limits: a crate that reads a repo file at run time through a path that is not a `../` literal in its
sources is not detected, and building a subset of packages can unify features differently from the
whole workspace. Run the full `cargo xtask ci` before a release.

## What the tests cover

- **Engine commands** (`crates/engine/src/tests*.rs`): every command family, including undo and
  redo, and that every menu entry resolves to a registered command.
- **Rendering** (`crates/render`): pixel checks on small compositions, for blend modes, masks,
  mattes, time remapping, 3D projection, lighting and shadows. Fast paths (the layer cache, the
  affine compositor, blurs, motion blur accumulation) are checked against their reference
  implementations.
- **Effects** (`crates/effects`): each effect has a determinism test and at least one behaviour
  test (identity at neutral settings, known pixel results, or simulations giving the same frame
  whether you seek straight to it or play up to it). A registry-wide test checks that effects
  which read the clock are declared time-dependent. `crates/effects/tests/sim_golden.rs` pins
  exact pixel hashes of the simulation effects the GPU shares plans with (CC Rainfall … Card
  Wipe) at several settings, times, bit depths and resolutions, so any change to their CPU output
  fails (re-pin with `SIM_GOLDEN_PRINT=1` after an intended change; pinned for aarch64 macOS,
  with the hashes another macOS release's libm gives for unchanged code in `LIBM_VARIANTS`).
- **Export** (`crates/export/tests`): every format is encoded and decoded back, checking frame
  count, size and pixels. When `ffmpeg`/`ffprobe` are installed they are used as an outside
  check; they are never linked or shipped.
- **Automation** (`crates/automation`, `apps/effectcraft-cli/tests`): MCP protocol round trips and
  the command-line tool's JSON output.
- **Interface** (`crates/ui-egui`): headless egui_kittest tests for panels and dialogs.
  `tests/ui_fingerprints.rs` renders every panel (wgpu, skipped without a GPU adapter), reduces it
  to a 32×20 grey grid and compares it with `tests/ui_fingerprints.txt` with a tolerance: icon and
  font tweaks pass, a blank, clipped or rearranged panel fails and its render is written to
  `target/tmp/ui-fingerprints/`. Re-pinning after an intended UI change is expected:
  `UI_FINGERPRINT_PRINT=1 cargo test -p effectcraft-ui-egui --test ui_fingerprints` (pinned for
  aarch64 macOS; elsewhere it only checks no panel renders flat).

## Looking at the interface without a window

```sh
cargo run -p effectcraft-ui-egui --example snapshot -- --out ui.png \
  --step '{"method":"engine.execute","params":{"command":"layer.select","params":{"layers":["#2"]}}}'
```

Each `--step` is a control-channel request; `{"method":"snap","params":{"path":"x.png"}}` writes an
intermediate image and `{"method":"pointer","params":{"x":…,"y":…,"click":true}}` clicks (or
`"down"` / `"up"` for a drag in steps). It runs the same offscreen app as `effectcraft-cli mcp --ui`
(`effectcraft_ui_egui::offscreen`).

## Benchmarks

```sh
cargo run --release -p effectcraft-cli -- bench --n 10 --play 30
```

prints per-layer and per-effect timings for one frame, and playback timings with and without the
layer cache.
