//! Shape layers in nested comps rasterised only where they can be seen.
//!
//! A nested (not collapsed) comp shows only its own frame, so the part of a large shape layer
//! that can never reach that frame need not be rasterised, processed or uploaded (a 3000×4000
//! backdrop in a 430×932 precomp: 184 MiB of RGBA f32 per frame for a few percent of it). The
//! layer's buffer is cut to a window of the buffer it would otherwise make (whole pixels of it,
//! so every kept pixel is the same pixel at the same place), grown by what its masks, effects and
//! resampling read around the frame and rounded out to a coarse grid (mip and downsampled-blur
//! alignment, and a cache key that holds while the layer moves a little). Whenever that margin
//! is not known, or something else may read the layer's pixels outside the frame, the whole
//! buffer is drawn as before.

use crate::motion_blur::MotionMemo;
use crate::placement::Window;
use crate::{EvalCtx, Image, Renderer, cache, styles};
use effectcraft_effects::Buf;
use effectcraft_geom::{Rect, vec2};
use effectcraft_project::{GroupKind, Layer, LayerSource};
use std::sync::atomic::{AtomicBool, Ordering};

/// Window edges snap to multiples of this many buffer pixels.
const GRID: u32 = 256;
/// Smaller buffers are drawn whole (not worth a separate cache key).
const MIN_PIXELS: u64 = 1 << 20;
/// A window keeping more than this share of the buffer isn't worth it.
const MAX_SHARE: f64 = 0.75;
/// Beyond this minification (buffer pixels per output pixel) the whole buffer is drawn (deep mip
/// levels depend on the buffer's size).
const MAX_DENSITY: f64 = 32.0;

/// Every shape layer drawn whole (the path before clipping), see [`set_shape_clip`].
static NO_CLIP: AtomicBool = AtomicBool::new(false);

/// Turn shape clipping off (`false`: every shape layer is rasterised whole, as before) or back on,
/// process-wide: A/B comparisons and tests against the whole raster.
#[doc(hidden)]
pub fn set_shape_clip(on: bool) {
    NO_CLIP.store(!on, Ordering::SeqCst);
}

impl<'a> Renderer<'a> {
    /// The window of a shape layer's whole raster buffer (see [`crate::shapes::buffer_rect`]) to
    /// rasterise: what can reach this nested comp's frame. `None` draws the whole buffer: content
    /// that held still over the last frames (cached whole, it is drawn once), a top-level,
    /// collapsed or Layer panel render, a 3D, stencil, adjustment, motion-blurred or styled layer,
    /// effects or masks whose reach is not known, a layer something else in the comp reads
    /// (track matte, effect layer parameter, `sampleImage`), or a window that saves little.
    /// `key` (the layer's content key, which fixes its buffer) memoises the buffer's rectangle.
    pub(crate) fn shape_clip(&self, ctx: &EvalCtx, layer: &Layer, key: Option<u64>) -> Option<Window> {
        if NO_CLIP.load(Ordering::Relaxed) {
            return None;
        }
        if !matches!(layer.source, LayerSource::Shape)
            || self.top
            || self.depth == 0
            || self.outer.is_some()
            || self.collapse3d.is_some()
            || self.layer_view.is_some()
            || self.canvas_origin != [0, 0]
        {
            return None;
        }
        if layer.is_3d()
            || layer.blend_mode.is_stencil()
            || layer.switches.adjustment
            || ctx.comp.renderer == effectcraft_project::Renderer::Advanced3D
            || self.blurred(ctx, layer)
            || styles::active(ctx, layer)
        {
            return None;
        }
        let contents = layer.props.sub("contents")?;
        let k = self.raster_scale(ctx, layer);
        let rect = || crate::shapes::buffer_rect(ctx, layer, contents, k);
        let (bw, bh, off) = match (self.cache, key) {
            (Some(c), Some(key)) => c.shape_rect(key, rect)?,
            _ => rect()?,
        };
        if (bw as u64) * (bh as u64) < MIN_PIXELS {
            return None;
        }
        // Static content stays whole in the cache (drawn once wherever it moves).
        if let (Some(c), Some(key)) = (self.cache, key)
            && c.content_held(cache::derive(ctx.comp_id.0, layer.id.0), key, ctx.time)
        {
            return None;
        }
        // Whole-buffer pixel → output pixel, and back.
        let buf = Buf { img: Image { width: bw, height: bh, data: vec![] }, offset: off, scale: k };
        let m = self.buf_matrix(ctx, layer, &buf, &MotionMemo::default());
        if !m.is_affine() {
            return None;
        }
        let inv = m.inverse()?;
        let o = inv.apply(vec2(0.0, 0.0));
        let density = [vec2(1.0, 0.0), vec2(0.0, 1.0)]
            .into_iter()
            .map(|v| {
                let p = inv.apply(v);
                (p.x - o.x).hypot(p.y - o.y)
            })
            .fold(0.0f64, f64::max);
        if !(density.is_finite() && density > 0.0 && density <= MAX_DENSITY) {
            return None;
        }
        // The frame (output pixels) with the resampling footprint around it, in buffer pixels.
        let s = self.opts.scale;
        let (cw, ch) = ((ctx.comp.width as f64 * s).round().max(1.0), (ctx.comp.height as f64 * s).round().max(1.0));
        let frame = Rect::new(-4.0, -4.0, cw + 4.0, ch + 4.0);
        let v = inv.map_rect(&frame);
        // What masks and effects read around a pixel (layer units), doubled for safety, plus the
        // filters' and mip levels' reach.
        let reach = self.mask_reach(ctx, layer)? + self.fx_reach(ctx, layer, k)?;
        let margin = 2.0 * reach * k + 8.0 + 4.0 * density.max(1.0);
        if !margin.is_finite() {
            return None;
        }
        let v = v.inflate(margin);
        if [v.x0, v.y0, v.x1, v.y1].iter().any(|x| !x.is_finite()) {
            return None;
        }
        let g = GRID as f64;
        let snap = |lo: f64, hi: f64, n: u32| -> (u32, u32) {
            let a = ((lo / g).floor() * g).clamp(0.0, n as f64) as u32;
            let b = ((hi / g).ceil() * g).clamp(0.0, n as f64) as u32;
            (a, b)
        };
        let (x0, x1) = snap(v.x0, v.x1, bw);
        let (y0, y1) = snap(v.y0, v.y1, bh);
        // Nothing reaches the frame: one (transparent, padding) pixel stands in.
        let win = if x1 <= x0 || y1 <= y0 { Window { x: 0, y: 0, w: 1, h: 1 } } else { Window { x: x0, y: y0, w: x1 - x0, h: y1 - y0 } };
        if win.is_whole((bw, bh)) || (win.w as f64 * win.h as f64) > MAX_SHARE * bw as f64 * bh as f64 {
            return None;
        }
        // Something else in the comp may read the layer's pixels anywhere (checked last: a scan
        // of the comp).
        if crate::layer_buf::working_read(ctx.comp, layer) {
            return None;
        }
        Some(win)
    }

    /// Layer units a layer's masks read around a pixel (feather and expansion); `None` when not
    /// known.
    fn mask_reach(&self, ctx: &EvalCtx, layer: &Layer) -> Option<f64> {
        let Some(masks) = layer.masks() else { return Some(0.0) };
        let mut reach = 0.0f64;
        for g in masks.groups().filter(|g| g.enabled && matches!(g.kind, GroupKind::Mask { .. })) {
            let f = ctx.v2(layer, g, "feather", [0.0; 2]);
            let e = ctx.f(layer, g, "expansion", 0.0);
            // A Gaussian feather of `f` has σ = f/2 (three σ), a linear one a box of f/2.
            reach = reach.max(1.5 * f[0].abs().max(f[1].abs()) + e.abs() + 2.0);
        }
        reach.is_finite().then_some(reach)
    }
}

/// The key of a clipped layer's content: its whole-buffer key and the window.
pub(crate) fn clipped_key(key: u64, w: Window) -> u64 {
    let packed = (w.x as u64 & 0xffff) | (w.y as u64 & 0xffff) << 16 | (w.w as u64 & 0xffff) << 32 | (w.h as u64 & 0xffff) << 48;
    cache::derive(cache::derive(key, 0x5c11_c0de), packed)
}
