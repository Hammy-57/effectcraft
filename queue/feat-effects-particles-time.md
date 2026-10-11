title: fix(effects): CC Particle World / Systems II replace their layer; Time effects see the effects before them
branch: upstream/feat-effects-particles-time
depends: none

## What
- **CC Particle World / CC Particle Systems II replace their layer with the particles.** Before, the particles were composited over the original layer using the Transfer Mode, so the solid they're applied to stayed visible. After Effects shows no trace of that solid. The Transfer Mode now only sets how particles combine with each other.
- **Time effects see the effects applied before them.** Echo, Posterize Time and the other Time effects used to read the layer at other times without any of its effects. After Effects renders them with the effects that come before them in the stack (for example Echo after a particle effect, or Posterize Time after Turbulent Displace). The host already limits the count to the effects before the one rendering.
- The particle golden hashes are re-pinned. The particle change alters their 112 CPU hashes on purpose; no other effect's hashes change.

## Why
These were measured against After Effects 2026 frames of third-party templates. Mean absolute error went from 0.0191 to 0.0130 and from 0.1177 to 0.0937 on two particle scenes, and from 0.1855 to 0.0071 on an Echo-after-particles scene. Fifteen other projects with a Time effect after another effect stay pixel-identical.

## How tested
- `cargo test -p effectcraft-effects` (lib, `particle_golden`, `timewarp_golden`).
- `cargo test -p effectcraft-render --lib tests_time`: a new test checks that Time effects see the effects before them.
- clippy `-D warnings` and rustfmt pass on this branch.
