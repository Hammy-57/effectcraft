title: perf(render): layer cache packs quantised 8/16 bpc entries losslessly instead of evicting
branch: upstream/perf-layer-cache-packed
depends: none

## What
Under budget pressure, the layer cache packs quantised 8 and 16 bpc entries instead of evicting them.

In 8 and 16 bpc projects every cached layer buffer is clamped and quantised to the bit depth, so each premultiplied channel is `k / levels` for an integer `k`. `PackedBuf` (new `packed.rs`) stores those integers as `u8` (255 levels) or `u16` (32768 levels), a quarter or half of the `f32` size, and unpacks them to the identical floats. `PackedBuf::pack` checks that every value round-trips bit for bit and declines otherwise (32 bpc, linear-blending copies, unquantised style passes), so packing never changes a pixel.

## Why
At the same memory budget the cache holds 2–4× as many 8/16 bpc layers before it has to evict and re-render.

## How tested
- `tests_packed.rs`: quantised buffers round-trip bit-exactly, unquantised buffers are not packed, the cache packs under pressure and counts the packed bytes, and a rendered 8 bpc layer packs losslessly.
- On this branch on top of `main`: `cargo clippy -p effectcraft-render --all-targets -- -D warnings` and `cargo test -p effectcraft-render` pass (render goldens and oracles included).

On the fork, render modules self-register through a build script. Here the last commit declares `packed` and `tests_packed` in `lib.rs`.
