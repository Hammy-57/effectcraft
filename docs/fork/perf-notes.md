# Performance notes (fork)

Decisions on render optimisations that were tried and **not** shipped because they could not be
made bit-exact. Fidelity comes first (`docs/gaps.md` G1): an optimisation ships only when every
rendered frame stays identical (exact `f32` equality) to the path it replaces, proven by a test
that keeps the old path as an oracle. Re-open these only with a design that removes the
counter-example below.

## Shipped: windowed isolated collapsed precomps (`collapse.rs`)

`draw_collapsed` (a collapsed precomp in a non-Normal mode, with a matte or Preserve
Transparency) now draws its nested layers into a temporary that covers only the window they can
reach, not a canvas-sized one. The window comes from the culling bounds (`Renderer::drawn_rects`,
the same bounds `off_canvas` uses), snapped outward to whole pixels and clamped to the canvas.
The nested renderer gets that window's integer origin (`Renderer::canvas_origin`), and every warp
(`composite_warp_at`), dissolve and footprint keeps the output's coordinates, so each pixel is
computed exactly as on the whole canvas. Any nested layer whose reach isn't known up front (3D,
adjustment, Wireframe, stencil modes, effects of unknown growth, collapsed precomps with effects,
unknown sources) falls back to the whole canvas.

Proof: `collapse::tests::isolated_collapsed_precomps_draw_into_their_window_bit_exactly` (20
cases × 4 render options × 2 times, against the `WHOLE` oracle; mutating the origin, the dissolve
coordinates or the bound's margin makes it fail).

Measured (`time_isolated_collapsed_precomp`, 200×150 Multiply precomp on a 1920×1080 comp):
temporary 1920×1080 (33.2 MB) → 190×140 (0.43 MB); about 18 → 16.5 ms per frame (the rest of
the frame dominates).

## Not shipped: windowed `draw_collapsed_fx` (effects on a collapsed precomp)

The effects run on the isolated canvas-sized image, and their result depends on its size.
Counter-example: Gaussian Blur (`raster::gaussian_blur`, three box passes). Each box pass is a
running sum, `acc += add - sub`; once the window has passed the content, the sum is not exactly
zero again but a cancellation residue. A 60-pixel premultiplied run at x 700..760 of a 1920-pixel
row, blurred with σ = 8/3 (support 7 px), leaves non-zero pixels on every one of the 1152
pixels from x 768 to 1919, e.g. x 768 = `[-2.18e-8, -3.74e-8, -1.87e-8, 1.96e-7]`. With an alpha
above 0 those pixels are blended into the comp (in 32 bpc nothing quantises them away), so a
temporary clipped to "content + blur growth" drops pixels the whole-canvas render changes. Most
other effects read the layer size or centre (point controls, Grid, radial effects, Fractal Noise)
and are canvas-size dependent by design. Kept on the whole canvas.

## Not shipped: clipping text/shape rasterisation to the visible region (`sources.rs`)

A text or shape layer enlarged far beyond the comp still rasterises its whole content buffer.
Rasterising only the part that can reach the comp would need the visible part to come out
exactly as the same pixels of the full buffer. Two steps break that:

1. **The rasteriser is not integer-translation invariant.** `effectcraft_path::fill_coverage`
   (tiny-skia mask fill: `f32` transforms, supersampled anti-aliasing) gives different coverage
   for the same path moved by a whole number of pixels. A circle of radius 40 at scale 2^(17/4)
   ≈ 19.03 (a 2000 % layer), filled into a 2000×2000 patch and again into the same patch moved
   937 px left (`translate(-1937, 0)` instead of `translate(-1000, 0)`), differs on 678 edge
   pixels: at buffer (938, 314) the coverage is 0.4980 in the full patch and 0.5608 in the
   window. Text already rasterises each run into a patch whose origin is clamped to the buffer
   (`text::draw_glyphs`), so a smaller buffer moves those origins and changes the glyph edges.
2. **The placement folds the buffer offset into the warp.** `place_matrix` ends with
   `translate(-buf.offset)`; a clipped buffer has `offset - i0`, and the product's translation
   (and the inverse the warp samples through) rounds differently, so bilinear/bicubic weights
   change in the last bits even for an integer `i0`.

A bit-exact version needs a rasteriser that writes a clip window of a fixed device grid (same
`f32` coordinates, only fewer pixels stored) and a warp that samples a source window at an
integer origin; both are larger changes than this leftover.

Measured today (`sources::tests::time_enlarged_text_and_shape`, 1920×1080 comp): a 2000 % text
line rasterises 8702×1131 (157 MB, ~0.25–0.3 s) at Full; a 1500 % star shape 4105×3906
(257 MB, ~0.27 s), capped below its on-screen scale (14.3 vs 15).

## Shipped: the rasterisation cap grows with the output (`sources.rs`)

`raster_scale` kept every continuous-raster buffer within 16 Mpx whatever the output size, so
on 8K renders large text and shapes were drawn below their on-screen scale. The cap is now
`max(16 Mpx, 1.25 × output pixels)` (the output being the frame a collapsed layer is drawn into),
applied only where the 16 Mpx cap held the step below the on-screen scale, and never past the
rasterisers' 16384-pixel buffer sides (which would cut the content off). Below about 12.8 Mpx
outputs (up to 4K UHD at Full) nothing changes. Frames change only where main was softer than
on-screen: `raster_scale_cap_grows_with_the_output_only_where_it_softened` checks every other
step is bit-identical to the fixed-cap oracle. At 8K (4× a 1080p comp): the 2000 % text goes from
30.3× to 42.5× (side limit; on-screen 80×), 282 → 556 MB; the 1500 % star from 14.3× to 23.0×
(on-screen 60×), 257 → 665 MB. A smaller viewer can't allocate less without the clipping above,
since lowering the step would soften the layer.
