title: test(fuzz): cargo-fuzz targets for untrusted input; cargo xtask fuzz
branch: upstream/fix-fuzz-targets
depends: none

## What
`fuzz/`: cargo-fuzz targets for the inputs EffectCraft doesn't control, and `cargo xtask fuzz` to run them.
- Nine targets: `project_open` (.ecproj JSON and XML, loaded and rendered), `lottie_import`, `svg_parse`, `psd_parse`, `pdf_parse` (PDF / AI / EPS), `interchange_import` (FCP7 XML, FCPXML, OTIO, EDL), `expr_eval`, `media_probe` (probe + decode) and `engine_command` (in-memory command families with fuzzed parameters on the demo project). File services deny every access; nothing touches the disk, network or processes.
- `cargo xtask fuzz --list | --seeds | [TARGET…] [--seconds N] [--jobs N]`; a seed corpus (`fuzz/corpus/*/seed-*`, attributed in ATTRIBUTION.md and sidecars); docs/testing.md "Fuzzing"; findings recorded under G3 in docs/gaps.md.
- The crate has its own `[workspace]`, so normal builds and CI never compile it (needs nightly + cargo-fuzz to fuzz).

Most of the line count is `fuzz/Cargo.lock` (generated) and seed files; the hand-written code is ~600 lines.

## Why
"Never crash" (G3): parsers of untrusted files had no fuzzing. The first minute per target found four problems; the two in this repo (PDF page-tree hang, huge path coordinates) are fixed in a separate PR (`fix(import): fuzzing finds…`).

## How tested
- On this branch: `cargo check --bins --examples` and `cargo clippy --bins --examples -- -D warnings` in `fuzz/` (stable), `cargo run --example seeds` (regenerates the corpus), `cargo clippy -p xtask -- -D warnings`, `cargo xtask assets` (ok), `cargo xtask fuzz --list`, rustfmt --check.
- The targets were fuzzed with `cargo +nightly fuzz` on the original branch; not re-fuzzed on this branch.
