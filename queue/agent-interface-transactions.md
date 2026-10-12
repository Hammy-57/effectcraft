title: fix(mcp): guard edits and retain recoverable session-aware results
branch: upstream/agent-interface-transactions
depends: upstream/mcp-compact-reads

## What
Agent edits now apply with explicit transaction, revision and session boundaries. Large results remain recoverable without repeating an action, including scripts and command batches.

- Validate key requests before mutation, apply keys/interpolation/readback in one undoable engine batch, and report applied/rolled-back outcomes. Revision guards cover convenience tools and long-job starts; polling and completion keep the same client without reusing a stale pre-edit revision.
- Preflight atomic batch commands against an explicit allowlist. External writes/project replacement are rejected before any step. Non-atomic batches preserve indexed successes/errors, returned file paths and explicit coverage uncertainty; command_batch keeps its continue/stop and separate undo behavior.
- Preserve project-open repairs, source version, handover, other-open-host information, and media warnings. Failed composition reads remain errors.
- Retain bounded completion receipts for oversized control replies, never evict in-flight entries, and refuse uncertain mutation replay after a process/session change. Replay protection is limited to retained cache lifetime.
- Join one exact canonical project host, verify its live path before switching, and expose session/build identity. Legacy relative host paths remain explicitly unresolved.
- Bound other large tool outputs and retain immutable results for get_result/resources retrieval, including private streamed spill storage. Failures and waiting/running state survive output handling; a retention failure never authorizes rerunning an edit.
- Declare retained-result and delivery-failure shapes in tool output schemas; validate real omitted-field replies against those schemas.
- Expose engine/app and MCP build provenance and scoped protocol features. Preserve complete output for one-shot CLI callers, which cannot page a handle after process exit.

## Why
Agents must not treat an error as proof that nothing happened, a partial list as the complete project, or a stale discovery record as the intended live session. The result used to plan the next edit now carries explicit completion, identity, continuation and recovery information.

## Validation
Fork implementation:
- cargo test -p effectcraft-automation --lib: 161 passed
- cargo test -p effectcraft-automation --test qa: 8 passed; QA follows retained snapshots when asserting complete history/template structures
- cargo test -p effectcraft-engine --lib commands::batch: 13 passed
- cargo test -p effectcraft-engine --lib build_info::tests: 4 passed
- cargo test -p effectcraft-engine --lib commands::query::inspection_tests: 6 passed
- cargo clippy -p effectcraft-automation -p effectcraft-engine --all-targets -- -D warnings: passed
- cargo test -p effectcraft-cli --no-default-features --bin effectcraft-cli one_shot_tool_keeps_complete_output_after_process_exit: 1 passed
- cargo clippy -p effectcraft-cli --no-default-features --all-targets -- -D warnings: passed
- Final cargo check on automation, engine and CLI (no default CLI UI feature): passed
- Exact upstream port: cargo check on automation, engine and CLI --no-default-features --all-targets: passed. Final schema follow-up: automation --all-targets check passed. Runtime test counts above belong to the fork implementation.

The upstream port is checked separately against the queued dependency. No full local CI, application build, release CLI build or live-project edits.

## Boundaries
Non-atomic file receipts report returned paths and explicitly mark incomplete coverage of untracked/failed external writers. Retained results expire on eviction/session end; large disk-backed results use exact root JSON chunks. Old running executables need the normal rebuild/reconnect workflow. This topic depends on mcp-compact-reads, which depends on the queued agents-own-undo prerequisite.
