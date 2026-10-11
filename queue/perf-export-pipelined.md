title: perf(export): render the next batch while encoding; frames in flight follow the memory budget
branch: upstream/perf-export-pipelined
depends: none

## What
- Export follows the Memory & CPU budget. `JobOptions::memory_budget` (default 1 GiB) sizes the job's layer cache. It also bounds the frames rendering at once: one per core (2–16), fewer when that many frames would exceed the budget. A 4K frame takes about 530 MB in flight, so two fit in 1 GiB. The app and the host pass the Memory & CPU layer-cache budget.
- Movie and WebM (VP9/AV1) exports render the next batch while the current one encodes and muxes. `Cx::batches` hands batches over in order from a scoped renderer thread through a rendezvous channel, so at most the batch being encoded and the next rendered one are held. Exports stay serial in passes, on wasm and on rayon workers.

## Why
Rendering and encoding used to alternate, leaving cores idle while a batch encoded. Large frames could also render 16 at a time whatever the memory available.

## How tested
- New `tests/pipelined.rs`: movie and WebM exports give the same bytes as the serial path (render a batch, encode it, render the next). An error or a stop on the encode side ends the export without hanging.
- `batch_size_follows_frame_bytes_and_budget` covers the batch sizes.
- On this branch on top of `main`: `cargo clippy --all-targets -- -D warnings` and `cargo test` pass for `effectcraft-export`, `effectcraft-engine` and `effectcraft-host` (rustfmt clean).

Ported from the fork. The only conflict was the audio start-tick fix, which is kept as on `main`.
