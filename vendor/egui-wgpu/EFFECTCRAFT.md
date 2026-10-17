# egui-wgpu 0.36.2, patched for EffectCraft

An unmodified copy of [egui-wgpu 0.36.2](https://crates.io/crates/egui-wgpu/0.36.2)
(<https://github.com/emilk/egui/tree/main/crates/egui-wgpu>, MIT OR Apache-2.0, see `LICENSE-MIT`), used
through `[patch.crates-io]` in the workspace `Cargo.toml`, with one change:

- `Renderer::update_buffers` no longer panics when it can't stage a frame's index or vertex data (out of
  video memory, or the device is lost): it logs a warning, skips that frame (`Renderer::render` draws
  nothing) and makes the buffer again next frame. The panic killed the app, and the document with it,
  when a shared 24 GB card filled up (CR1, 17 Oct 2026). Lines marked `EffectCraft patch`.

Regression test: `the_window_renderer_skips_a_frame_it_cannot_stage` in `crates/ui-egui/src/lib.rs`.
When eframe/egui move past 0.36.2, check whether upstream still panics there, and update or drop this copy.
