# Parity with After Effects

How close EffectCraft is to After Effects 2026, feature by feature, and how much work is left.
It measures breadth, not real-world readiness: read [gaps.md](gaps.md) next to it.

The parity record lives in [`docs/parity/`](parity/), one file per part, so parallel work never
edits the same file:

| File | What it holds | Who edits it |
|---|---|---|
| [`parity/overview.md`](parity/overview.md) | **The current status**: headline numbers, partial features, previous audits | audits |
| [`parity/areas/<area>.md`](parity/areas/) | One area of the per-area table (weighted parity, remaining agent-hours, biggest gaps) | work in that area |
| [`parity/updates/<task>.md`](parity/updates/) | What one task (milestone) changed, as a `## Update: <task> <title>` section | the task that landed it (add a new file) |
| [`parity/effects.md`](parity/effects.md) | Effects still missing and the M9.11 additions | effects work |
| [`parity/original-audit.md`](parity/original-audit.md) | The original audit of 2 October 2026 and its first updates, kept for history | nobody |
| [`parity/gaps.md`](parity/gaps.md) | The historical list of highest-value gaps | audits |

`cargo xtask parity` checks the parts and assembles them into one page (`target/parity.md`, or
`--out <file>`): the overview, the original audit, the per-area table, the effects, the updates in
task order and the gaps. Don't edit this index for a feature; add or edit your part instead.
