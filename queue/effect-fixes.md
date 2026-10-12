title: fix(effects): CC Particle World / Systems II replace their layer; Time effects see earlier effects
branch: upstream/effect-fixes
depends: none

## What
Two effect fixes found comparing against After Effects 2026 renders:

- **CC Particle World / CC Particle Systems II replace their layer with the particles.** They were
  composited over the original layer by the Transfer Mode, so the solid they're applied to stayed
  visible (a red rectangle around a flame, black blocks over light). The Transfer Mode now only
  sets how particles meet each other. Their 112 pinned CPU golden hashes change on purpose (re-pinned
  in its own commit); no other effect changed.
- **Time effects see the effects applied before them.** Echo, Posterize Time and the other Time
  effects read the layer at other times without its earlier effects; After Effects renders them
  with those effects (e.g. Echo after CC Particle Systems II, Posterize Time after Turbulent
  Displace). The host already bounds the count to the effects before the one rendering.

## Why
Mean absolute error vs After Effects on test projects: 0.0191 → 0.0130, 0.1177 → 0.0937, and
0.1855 → 0.0071 for one using both; 15 other projects with a Time effect after another effect are
pixel-identical. Split out of #858 as asked.

## How tested
- `crates/render/src/tests_time.rs` (new case) and `crates/effects/tests/particle_golden.rs`
  (re-pinned); written with the changes, not re-run on this branch.
- `rustfmt --check` and `cargo check --all-targets` on the affected crates on current upstream
  `main`.
