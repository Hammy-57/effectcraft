//! Where a processed layer buffer lands in the output: the layer → output matrices, the
//! region of interest, motion-blur sub-samples, sampling, and the changed region.

use crate::color::Region;
use crate::motion_blur::MotionMemo;
use crate::{EvalCtx, Image, Renderer, color};
use effectcraft_color::BlendMode;
use effectcraft_effects::Buf;
use effectcraft_geom::{Mat3, vec2};
use effectcraft_project::{Layer, Quality, Sampling};
use effectcraft_raster::{MipChain, WarpOpts, accumulate_warp_mips, composite_warp_mips};

impl<'a> Renderer<'a> {
    pub(crate) fn sampling(&self, layer: &Layer) -> effectcraft_raster::Sampling {
        if self.quality(layer) == Quality::Draft {
            // Draft quality: no interpolation (nearest neighbour), as After Effects' Draft.
            effectcraft_raster::Sampling::Nearest
        } else if self.opts.draft {
            effectcraft_raster::Sampling::Bilinear
        } else if layer.switches.sampling == Sampling::Bicubic {
            effectcraft_raster::Sampling::Bicubic
        } else {
            effectcraft_raster::Sampling::Bilinear
        }
    }

    /// The region of interest of a top-level 2D frame: (x, y) offset of the output in output
    /// pixels and its size. `None` renders the whole comp (no ROI, or a nested comp).
    pub(crate) fn roi_offset(&self) -> Option<(f64, f64, u32, u32)> {
        let r = self.opts.roi.filter(|_| self.top)?;
        let s = self.opts.scale;
        if r[2] <= 0.0 || r[3] <= 0.0 {
            return None;
        }
        Some(((r[0] * s).round(), (r[1] * s).round(), ((r[2] * s).round() as u32).max(1), ((r[3] * s).round() as u32).max(1)))
    }

    /// The region of interest's offset in output pixels for 3D projection (0 without one).
    pub(crate) fn out_offset(&self) -> (f64, f64) {
        self.roi_offset().map_or((0.0, 0.0), |(x, y, _, _)| (x, y))
    }

    /// Scaled comp pixel → output pixel: the region of interest's offset.
    fn out_matrix(&self) -> Mat3 {
        match self.roi_offset() {
            Some((x, y, _, _)) => Mat3::translate(vec2(-x, -y)),
            None => Mat3::IDENTITY,
        }
    }

    /// Layer → (top-level) comp matrix at the context time: through the collapsed precomps.
    pub(crate) fn comp_matrix(&self, ctx: &EvalCtx, layer: &Layer) -> Mat3 {
        let (l2c, _) = ctx.layer_to_comp(layer);
        self.outer.map_or(l2c, |o| o * l2c)
    }

    /// Buffer pixel → output pixel matrix for the layer at the context time.
    pub(crate) fn buf_matrix(&self, ctx: &EvalCtx, layer: &Layer, buf: &Buf, mo: &MotionMemo) -> Mat3 {
        self.place_matrix(mo.still(|| self.comp_matrix(ctx, layer)), buf)
    }

    /// Buffer pixel → output pixel matrix for a layer → (top-level) comp matrix.
    pub(crate) fn place_matrix(&self, l2c: Mat3, buf: &Buf) -> Mat3 {
        let s = self.opts.scale;
        self.out_matrix()
            * Mat3::scale(vec2(s, s))
            * l2c
            * Mat3::scale(vec2(1.0 / buf.scale, 1.0 / buf.scale))
            * Mat3::translate(vec2(-buf.offset[0], -buf.offset[1]))
    }

    /// Canvas pixels that compositing `buf` for `layer` can change: the transformed buffer's
    /// bounds plus the resampling footprint. Stencil/silhouette modes, motion blur and
    /// perspective fall back to the whole canvas.
    ///
    /// A quantised buffer placed pixel-exactly (integer translation, Normal, 100%) over a blank
    /// canvas leaves exactly its own (quantised) pixels: nothing to re-quantise.
    pub(crate) fn footprint(&self, ctx: &EvalCtx, layer: &Layer, buf: &Buf, opacity: f32, blank: bool, mo: &MotionMemo) -> Region {
        if layer.blend_mode.is_stencil() || self.mb_samples(ctx, layer, buf, mo) > 1 {
            return Region::Full;
        }
        let m = self.buf_matrix(ctx, layer, buf, mo);
        if !m.is_affine() {
            return Region::Full;
        }
        let matte = layer.track_matte.is_some_and(|tm| tm.layer != layer.id && ctx.comp.layer(tm.layer).is_some());
        let k = &m.0;
        let integral = |v: f64| v == v.round();
        if blank
            && layer.blend_mode == BlendMode::Normal
            && opacity == 1.0
            && !matte
            && !layer.preserve_transparency
            && self.pipe.to_blend().is_none()
            && k[0][0] == 1.0
            && k[1][1] == 1.0
            && k[0][1] == 0.0
            && k[1][0] == 0.0
            && integral(k[0][2])
            && integral(k[1][2])
        {
            return Region::Empty;
        }
        let (w, h) = (buf.img.width as f64, buf.img.height as f64);
        let c = [(0.0, 0.0), (w, 0.0), (0.0, h), (w, h)].map(|(x, y)| m.apply(vec2(x, y)));
        let (x0, x1) = (c.iter().map(|p| p.x).fold(f64::INFINITY, f64::min), c.iter().map(|p| p.x).fold(f64::NEG_INFINITY, f64::max));
        let (y0, y1) = (c.iter().map(|p| p.y).fold(f64::INFINITY, f64::min), c.iter().map(|p| p.y).fold(f64::NEG_INFINITY, f64::max));
        color::PxRect::covering(x0, y0, x1, y1, 3.0).map_or(Region::Full, Region::Rect)
    }

    /// How a processed layer buffer lands in the output: one buffer → output matrix, or one per
    /// motion-blur sub-sample (averaged), with the layer's sampling filter and dissolve seed.
    pub fn placement(&self, ctx: &EvalCtx, layer: &Layer, buf: &Buf) -> Placement {
        self.placement_memo(ctx, layer, buf, &MotionMemo::default())
    }

    /// [`Self::placement`] sharing the layer's matrices with the draw's other buffers and steps.
    pub(crate) fn placement_memo(&self, ctx: &EvalCtx, layer: &Layer, buf: &Buf, mo: &MotionMemo) -> Placement {
        let samples = self.mb_samples(ctx, layer, buf, mo);
        let (sampling, seed) = (self.sampling(layer), layer.id.0 as u32);
        if samples <= 1 {
            return Placement { matrices: vec![self.buf_matrix(ctx, layer, buf, mo)], sampling, seed };
        }
        let sh = self.shutter(ctx);
        let matrices = (0..samples).map(|i| self.buf_matrix_at(ctx, layer, buf, sh.offset(i, samples), mo)).collect();
        Placement { matrices, sampling, seed }
    }

    /// Draw a processed layer into `target` (transform, motion blur) with `mode`/`opacity`.
    pub(crate) fn place(&self, ctx: &EvalCtx, layer: &Layer, buf: &Buf, target: &mut Image, mode: BlendMode, opacity: f32) {
        self.place_memo(ctx, layer, buf, target, mode, opacity, &MotionMemo::default());
    }

    /// [`Self::place`] sharing the layer's matrices through `mo` (see [`MotionMemo`]).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn place_memo(&self, ctx: &EvalCtx, layer: &Layer, buf: &Buf, target: &mut Image, mode: BlendMode, opacity: f32, mo: &MotionMemo) {
        let pl = self.placement_memo(ctx, layer, buf, mo);
        let opts = WarpOpts { sampling: pl.sampling, opacity, mode, seed: pl.seed, clip: None };
        // One minification chain for every sub-sample (built only when the layer is shrunk).
        let mips = MipChain::new(&buf.img);
        if let [m] = pl.matrices.as_slice() {
            composite_warp_mips(target, &mips, m, &opts);
            return;
        }
        // Sub-samples sum straight into one buffer (each warp is row-parallel).
        let mut acc = Image::new(target.width, target.height);
        let k = 1.0 / pl.matrices.len() as f32;
        for m in &pl.matrices {
            accumulate_warp_mips(&mut acc, &mips, m, opts.sampling, k);
        }
        target.blend_from(&acc, mode, opacity, pl.seed);
    }
}

/// Where a layer buffer lands (see [`Renderer::placement`]).
#[derive(Clone, Debug)]
pub struct Placement {
    /// Buffer pixel → output pixel; several = motion-blur sub-samples (equal weights).
    pub matrices: Vec<Mat3>,
    pub sampling: effectcraft_raster::Sampling,
    /// Dissolve noise seed.
    pub seed: u32,
}
