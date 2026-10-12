title: fix(mcp): make agent inspection and edit feedback exact and bounded
branch: upstream/mcp-compact-reads
depends: upstream/agents-control-robustness

## What
Editing agents now receive exact, size-bounded inspection data and accurate edit/error confirmations. A timed property write reports the actual snapped comp and layer times instead of reading back at the wrong clock; script failures set MCP isError while preserving output and partial-edit details.

- Reject ambiguous composition, layer, effect/group and display-name property targets before mutation, with candidate IDs and context. Numeric IDs, @uid, explicit occurrences and canonical property paths remain available.
- Inspect the targeted composition's time consistently. Expose raw and evaluated values, expression errors, stable property UIDs and effective enabled state.
- Preserve exact numeric values in compact rows. Bound default JSON pages to 7,000 serialized bytes, place completeness metadata before nested lists, and recover oversized fields through exact streamed get_field chunks. full:true remains an explicit unbounded compatibility route.
- Prefer comp-time animation recipes and document stretching, reversal and frame snapping.
- Include the owner's explicitly requested display-name credit.

## Why
Large responses can be clipped before an agent sees all layers or keyframes. Separately, misleading success, target or time information causes an agent to edit the wrong object or “repair” an already-correct key. These changes make the information used for the next edit explicit and testable.

## How tested
With four build jobs and a shared warm target:
- cargo check -p effectcraft-automation -p effectcraft-engine --all-targets
- cargo test -p effectcraft-automation --lib: 113 passed
- cargo test -p effectcraft-automation --test qa: 8 passed
- cargo test -p effectcraft-engine --lib commands::target_refs::tests: 10 passed
- cargo test -p effectcraft-engine --lib commands::query::inspection_tests: 3 passed
- cargo test -p effectcraft-engine --lib tests_key_time: 7 passed
- cargo clippy -p effectcraft-automation -p effectcraft-engine --all-targets -- -D warnings
- Post-integration cargo check on both touched crates.

No full CI, application build, release CLI build or live-project edits were performed. This updates the existing compact-read topic; follow-up transactional/retry/session work is separate.

The upstream topic also merges its queued `agents-control-robustness` prerequisite. Its port preserves upstream's manual command registry and scheduled-task behavior. The exact port passed `cargo check -p effectcraft-automation -p effectcraft-engine --all-targets` and formatting checks; the runtime test counts above were measured on the fork implementation.
