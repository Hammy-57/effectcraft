title: feat(fidelity): effect fidelity harness against After Effects; CompItem.saveFrameToPng
branch: upstream/feat-effect-fidelity-harness
depends: upstream/agents-control-robustness

## What
An effect-fidelity harness against After Effects:
- `examples/effects-fidelity.jsx` builds 23 original effect scenes with the After Effects scripting model, so the same file runs in After Effects and in EffectCraft (`effectcraft-cli script`).
- `examples/check-ae-effects.mjs` renders both, compares the frames (PSNR) and writes `docs/fidelity/effects.json`. Frames and diff heatmaps go to the gitignored `plan/aftereffects/ref/effects/`; none are committed.
- Scripts get `CompItem.saveFrameToPng`, as in After Effects.

## Why
It gives a repeatable, measured check of effect parity, rather than judging by eye (gaps.md G1).

## How tested
- `cargo test -p effectcraft-script` (including the new `save_frame_to_png`).
- clippy `-D warnings` and rustfmt pass on this branch.
- Upstream port: `saveFrameToPng` goes through `comp.saveFrameAs`.
- Not yet run against After Effects.
