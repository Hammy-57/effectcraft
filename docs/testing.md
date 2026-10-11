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
  `UI_FINGERPRINT_PRINT=1 cargo test -p effectcraft-ui-egui --test ui ui_fingerprints::` (pinned for
  aarch64 macOS; elsewhere it only checks no panel renders flat).

## One test binary per crate

Each `tests/*.rs` file is normally its own test binary, and each one links the crate and all its
dependencies again (46 copies of the whole app for `crates/ui-egui`). The crates with many
integration tests set `autotests = false` and build them as one binary instead, whose root declares
every file as a module:

| Crate | Binary | Root |
| --- | --- | --- |
| `effectcraft-ui-egui` | `ui` | `crates/ui-egui/tests/ui/main.rs` |
| `effectcraft-host` | `host` | `crates/host/tests/host/main.rs` |
| `effectcraft-export` | `export` | `crates/export/tests/export/main.rs` |
| `effectcraft-effects` | `effects` | `crates/effects/tests/effects/main.rs` |

```sh
cargo test -p effectcraft-ui-egui --test ui                      # all of them
cargo test -p effectcraft-ui-egui --test ui ui_viewer::          # one file (a name filter)
cargo test -p effectcraft-ui-egui --test ui ui_tabs:: -- --ignored   # its opt-in snapshots
cargo nextest run -p effectcraft-host --test host repros::
```

A new `tests/x.rs` needs `#[path = "../x.rs"] mod x;` in the root; until then cargo ignores the
file and the root's `every_test_file_is_a_module` test fails. Tests of different files now share a
process and run in parallel: a ui test that creates a GPU device holds `crate::gpu_lock()`
(wgpu's GL backend on Mesa crashes with concurrent devices), and process-wide state (env vars,
fixed temp file names) must not be shared between tests.

Dev and test builds carry line tables only (`debug = "line-tables-only"`, dependencies none):
backtraces keep file and line, links are smaller. `CARGO_PROFILE_DEV_DEBUG=true` brings full debug
info back for the workspace crates when you need a debugger.

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

### Single-point tracking budget

The normal suite keeps the 1080p tracking accuracy check (error below 0.25 pixels). Its wall-clock
budget runs separately because competing tests and builds can inflate elapsed time without a
tracking regression. On an otherwise idle machine, run the exact benchmark alone:

```sh
cargo test -p effectcraft-track --lib --profile ci tests::performance_1080p_single_point -- --exact --ignored --test-threads=1 --nocapture
```

The original budget remains **below 100 ms/frame**, including tracker initialization and five tracking
steps but excluding synthetic frame generation. The benchmark also checks the same accuracy fixture.
`--test-threads=1` isolates test execution within this process; stop competing builds and workloads too.

### Render benchmark (`cargo xtask bench`)

```sh
nice -n 10 cargo xtask bench --bin target/release/effectcraft-cli   # or no --bin: one `--profile local` build into target/bench
nice -n 10 cargo xtask bench --quick --compare before.jsonl        # 8 frames instead of 60, % change against an earlier run
cargo xtask bench --project F.ecproj --comp "Main"                 # any project's comp
```

measures memory, CPU and wall time of whole `effectcraft-cli` runs, so every optimisation has a
before and after number. It generates four deterministic scenarios into `target/bench/projects`
with engine commands (no committed media): `demo` (the built-in demo comp), `layers` (1080p,
100 solid, text and shape layers with Gaussian Blur, Glow, Levels, Curves, Drop Shadow and
keyframes), `precomps` (static and animated precomps nested three deep, each used several times)
and `footage` (a rendered PNG sequence and H.264 clip on several 1080p layers, plus a 4K still).
Each is opened (`open`), rendered at Full and Half (PNG sequence, deleted afterwards) and as one
frame. `/usr/bin/time -l` on macOS gives peak memory footprint, max RSS, user + sys CPU and
instructions retired; `/usr/bin/time -v` on Linux gives max RSS and CPU; elsewhere wall time only.

It prints a table and appends one JSON line per run to `target/bench/results.jsonl` (git rev,
date, load average, scenario, settings, metrics); `--compare OLD.jsonl` prints the % change against
the latest matching runs there, `--compare OLD --with NEW` compares two files without rendering.
The table notes the 1-minute load average: on a busy machine wall time is noise, so compare
peak memory and instructions retired, and rerun when the load differs a lot.
