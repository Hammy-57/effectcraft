# Repro scripts

Every `*.jsonl` here (and one folder down) is a replay script run by `effectcraft-host`'s
`repros` test: crash repros, behaviour expectations and `golden` frames. Format:
`crates/engine/src/replay.rs`; how agents make them: [docs/agents.md](../../docs/agents.md).

- `cargo xtask repros` replays them all, plus the gitignored `plan/repros/` (users' own projects,
  which can't be committed) and any `--dir DIR`.
- `cargo xtask repros --bless` writes the `golden` PNGs instead of comparing them. Do it only for
  a change that is meant to alter a render, look at the new frames, and commit them with it.
- One script: `effectcraft-cli replay tests/repros/x.jsonl [--bless]`.

Turning a user's file into a regression check: put the project (and its footage) in
`plan/repros/<name>/`, write `<name>.jsonl` starting `{"start": {"project": "<name>.ecproj"}}`,
add `expect` lines for the values the bug got wrong and `golden` lines for the frames, run
`cargo xtask repros --bless` once on a build that has the fix, and check the PNGs by eye.
When the file is ours to share (original content, no Adobe or third-party assets), it can live
here instead, with its goldens in `goldens/`.
