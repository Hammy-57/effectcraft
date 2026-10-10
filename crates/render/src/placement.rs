//! Where a processed layer buffer lands in the output: the layer → output matrices, the
//! region of interest, motion-blur sub-samples, sampling, and the changed region.

use crate::color::Region;
use crate::motion_blur::MotionMemo;
use crate::{EvalCtx, Image, Renderer, color, source_size};
use effectcraft_color::BlendMode;
use effectcraft_effects::Buf;
use effectcraft_geom::{Mat3, Rect, vec2};
use effectcraft_project::{ItemKind, Layer, LayerSource, Quality, Sampling};
use effectcraft_raster::{MipChain, WarpOpts, accumulate_warp_at, composite_warp_at, pool, warp_bounds};

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
        self.place_at(ctx, layer, buf, target, [0, 0], mode, opacity, mo);
    }

    /// [`Self::place_memo`] into `target`, the window of the canvas at `origin` (see
    /// [`Self::reach`]): exactly the canvas's pixels there.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn place_at(
        &self,
        ctx: &EvalCtx,
        layer: &Layer,
        buf: &Buf,
        target: &mut Image,
        origin: [u32; 2],
        mode: BlendMode,
        opacity: f32,
        mo: &MotionMemo,
    ) {
        let pl = self.placement_memo(ctx, layer, buf, mo);
        let opts = WarpOpts { sampling: pl.sampling, opacity, mode, seed: pl.seed, clip: None };
        // One minification chain for every sub-sample (built only when the layer is shrunk).
        let mips = MipChain::new(&buf.img);
        if let [m] = pl.matrices.as_slice() {
            composite_warp_at(target, origin, &mips, m, &opts);
            return;
        }
        // Sub-samples sum straight into one buffer (each warp is row-parallel), as large as they
        // reach (stencil modes also change what they don't cover: the whole target).
        let whole = Window { x: origin[0], y: origin[1], w: target.width, h: target.height };
        let win = if mode.is_stencil() { whole } else { whole.clip(reach_of(&[(&pl, buf)])) };
        if win.is_empty() {
            return;
        }
        let mut acc = pool::zeroed(win.w, win.h);
        let k = 1.0 / pl.matrices.len() as f32;
        for m in &pl.matrices {
            accumulate_warp_at(&mut acc, win.origin(), &mips, m, opts.sampling, k);
        }
        target.blend_from_at(origin, &acc, win.origin(), mode, opacity, pl.seed);
        pool::recycle(acc);
    }

    /// Whether a 2D layer certainly draws nothing on a `w`×`h` canvas, before its source is
    /// rendered: its source rectangle (with a margin for the buffer's rounding and the filters)
    /// lands outside the canvas. Only layers whose pixels stay inside a rectangle known up front
    /// qualify: solids, precomps and raster footage (their source size), text and shapes (the
    /// buffer their rasteriser makes); without effects or layer styles (effects may grow the
    /// buffer or read other layers), not motion blurred, not in a stencil mode (which changes
    /// the comp outside the layer) and not seen through a perspective.
    pub(crate) fn off_canvas(&self, ctx: &EvalCtx, layer: &Layer, (w, h): (u32, u32), mo: &MotionMemo) -> bool {
        #[cfg(test)]
        if tests::NO_CULL.with(std::cell::Cell::get) {
            return false;
        }
        let source = match &layer.source {
            LayerSource::Solid { .. } | LayerSource::Comp { .. } => Bounded::Rect,
            LayerSource::Footage { item } if matches!(self.project.item(*item).map(|i| &i.kind), Some(ItemKind::Footage(f)) if !crate::is_vector_footage(f)) => {
                Bounded::Rect
            }
            LayerSource::Text | LayerSource::Shape => Bounded::Drawn,
            _ => return false,
        };
        if layer.is_3d()
            || layer.blend_mode.is_stencil()
            || layer.effects().is_some_and(|g| !g.children.is_empty())
            || layer.layer_styles().is_some()
            || self.blurred(ctx, layer)
        {
            return false;
        }
        let s = self.opts.scale;
        let m = self.out_matrix() * Mat3::scale(vec2(s, s)) * mo.still(|| self.comp_matrix(ctx, layer));
        if !m.is_affine() {
            return false;
        }
        let r = match source {
            Bounded::Rect => {
                let (sw, sh) = source_size(self.project, layer);
                if sw == 0 || sh == 0 {
                    return false;
                }
                let pad = (0.1 * sw.max(sh) as f64).max(4.0);
                Rect::new(-pad, -pad, sw as f64 + pad, sh as f64 + pad)
            }
            Bounded::Drawn => {
                // Measuring text or shapes costs a layout: only for layers whose anchor point is
                // off the canvas (most visible layers draw around it).
                let a = layer.transform().map(|tr| ctx.v3(layer, tr, "anchor", [0.0; 3])).unwrap_or([0.0; 3]);
                let p = m.apply(vec2(a[0], a[1]));
                if !(p.x < 0.0 || p.y < 0.0 || p.x > w as f64 || p.y > h as f64) {
                    return false;
                }
                let Some(r) = self.drawn_rect(ctx, layer) else { return false };
                r
            }
        };
        let r = m.map_rect(&r).inflate(4.0);
        // NaN bounds (a degenerate transform) never count as outside.
        let outside = r.x1 < 0.0 || r.y1 < 0.0 || r.x0 > w as f64 || r.y0 > h as f64;
        outside && [r.x0, r.y0, r.x1, r.y1].iter().all(|v| !v.is_nan())
    }

    /// Layer-space rectangle of the buffer a text or shape layer's source draws into (see
    /// [`crate::text::render`] and [`crate::shapes::buffer_rect`]); `None` when it draws nothing
    /// or is not known.
    fn drawn_rect(&self, ctx: &EvalCtx, layer: &Layer) -> Option<Rect> {
        let k = self.raster_scale(ctx, layer);
        if !(k.is_finite() && k > 0.0) {
            return None;
        }
        let (bw, bh, off) = match &layer.source {
            LayerSource::Shape => crate::shapes::buffer_rect(ctx, layer, layer.props.sub("contents")?, k)?,
            LayerSource::Text => {
                // `text::render`'s buffer: every glyph's bounds grown by its stroke and blur.
                let geom = crate::text::text_geom(ctx, layer)?;
                let b = geom
                    .glyphs
                    .iter()
                    .filter_map(|g| {
                        let pad = g.stroke_width + 2.0 + g.xf.blur[0].max(g.xf.blur[1]) * 1.5;
                        effectcraft_path::bounds(&[g.path()]).map(|b| b.inflate(pad, pad))
                    })
                    .reduce(|a, b| a.union(b))?;
                let b = b.intersect(kurbo::Rect::new(-20000.0, -20000.0, 20000.0, 20000.0));
                let bw = ((b.width() * k).ceil() as u32 + 4).clamp(1, 16384);
                let bh = ((b.height() * k).ceil() as u32 + 4).clamp(1, 16384);
                (bw, bh, [-b.x0 * k + 2.0, -b.y0 * k + 2.0])
            }
            _ => return None,
        };
        let r = Rect::new(-off[0] / k, -off[1] / k, (bw as f64 - off[0]) / k, (bh as f64 - off[1]) / k);
        [r.x0, r.y0, r.x1, r.y1].iter().all(|v| v.is_finite()).then_some(r)
    }

    /// The window of a `w`×`h` canvas that drawing `bufs` for `layer` can change: their
    /// transformed bounds over every motion-blur sub-sample. Callers that clear or tint outside
    /// the layer (stencil modes) need the whole canvas instead.
    pub(crate) fn reach(&self, ctx: &EvalCtx, layer: &Layer, bufs: &[&Buf], mo: &MotionMemo, (w, h): (u32, u32)) -> Window {
        let whole = Window { x: 0, y: 0, w, h };
        #[cfg(test)]
        if crate::tests_footprint::full_windows() {
            return whole;
        }
        let pls: Vec<Placement> = bufs.iter().map(|b| self.placement_memo(ctx, layer, b, mo)).collect();
        let items: Vec<(&Placement, &Buf)> = pls.iter().zip(bufs.iter().copied()).collect();
        whole.clip(reach_of(&items))
    }
}

/// What bounds a culled layer's pixels (see [`Renderer::off_canvas`]).
#[derive(Clone, Copy)]
enum Bounded {
    /// Its source rectangle.
    Rect,
    /// The buffer its text or shapes are rasterised into.
    Drawn,
}

/// The union of the warps' bounds of these placed buffers (output pixels).
fn reach_of(items: &[(&Placement, &Buf)]) -> Rect {
    let mut r = Rect::EMPTY;
    for (pl, buf) in items {
        for m in &pl.matrices {
            r = r.union(&warp_bounds(m, buf.img.width, buf.img.height));
        }
    }
    r
}

/// A window of the canvas: `w`×`h` pixels from (`x`, `y`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Window {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

impl Window {
    pub(crate) fn origin(&self) -> [u32; 2] {
        [self.x, self.y]
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.w == 0 || self.h == 0
    }

    /// Whether this is the whole `w`×`h` canvas.
    pub(crate) fn is_whole(&self, (w, h): (u32, u32)) -> bool {
        self.x == 0 && self.y == 0 && self.w == w && self.h == h
    }

    /// The canvas region this window covers (the whole canvas as [`Region::Full`]).
    pub(crate) fn region(&self, size: (u32, u32)) -> Region {
        if self.is_empty() {
            Region::Empty
        } else if self.is_whole(size) {
            Region::Full
        } else {
            Region::Rect(color::PxRect { x0: self.x, y0: self.y, x1: self.x + self.w, y1: self.y + self.h })
        }
    }

    /// The part of this window inside `r` (whole pixels, `r` rounded out).
    pub(crate) fn clip(&self, r: Rect) -> Window {
        let (x1, y1) = (self.x as f64 + self.w as f64, self.y as f64 + self.h as f64);
        let c = |v: f64, lo: f64, hi: f64| if v.is_nan() { lo } else { v.clamp(lo, hi) };
        let (a0, a1) = (c(r.x0.floor(), self.x as f64, x1), c(r.x1.ceil(), self.x as f64, x1));
        let (b0, b1) = (c(r.y0.floor(), self.y as f64, y1), c(r.y1.ceil(), self.y as f64, y1));
        if a1 <= a0 || b1 <= b0 {
            return Window { x: self.x, y: self.y, w: 0, h: 0 };
        }
        Window { x: a0 as u32, y: b0 as u32, w: (a1 - a0) as u32, h: (b1 - b0) as u32 }
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

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use effectcraft_color::Label;
    use effectcraft_keyframe::{TextDoc, Value};
    use effectcraft_project::build::{self, Ids};
    use effectcraft_project::{BitDepth, Comp, ItemId, ItemKind, Layer, LayerSource, Project, Solid};
    use effectcraft_time::{FrameRate, Tick};

    use crate::{EvalCtx, Image, NoFootage, RenderOpts, Renderer};

    thread_local! {
        /// Draw every layer (the path without culling).
        pub(super) static NO_CULL: Cell<bool> = const { Cell::new(false) };
    }

    fn setup() -> (Project, ItemId, Comp) {
        let mut p = Project::default();
        p.settings.bit_depth = BitDepth::Bpc8;
        let comp = Comp::new(200, 100, FrameRate::FPS_30, Tick::from_seconds_f64(2.0));
        let cid = p.add_item("Comp", Label::Sandstone, None, ItemKind::Comp(comp.clone().into()));
        (p, cid, comp)
    }

    fn text(p: &mut Project, comp: &Comp, stroke: f64) -> Layer {
        let mut l = build::layer(p, comp, "Text", LayerSource::Text, (comp.width, comp.height), None);
        let doc = TextDoc { text: "Hgj".into(), size: 40.0, stroke_width: stroke, apply_stroke: stroke > 0.0, ..Default::default() };
        l.props.prop_mut("text/sourceText").unwrap().value = Value::Text(Box::new(doc));
        l
    }

    /// A text layer whose characters are blurred by an animator.
    fn blurred_text(p: &mut Project, comp: &Comp) -> Layer {
        let mut l = text(p, comp, 0.0);
        let mut next = p.next_id;
        let mut ids = Ids(&mut next);
        let props = build::text_anim_props(&mut ids, "blur", false);
        let g = build::text_animator(&mut ids, "Animator", props);
        p.next_id = next;
        l.props.group_mut("text/animators").unwrap().children.push(g.into());
        l.props.group_mut("text/animators/#1/properties").unwrap().get_mut("blur").unwrap().value = Value::Vec2([6.0, 3.0]);
        l
    }

    /// A shape layer: a stroked, filled star whose group sits away from the layer's anchor.
    fn shape(p: &mut Project, comp: &Comp, offset: [f64; 2]) -> Layer {
        let mut l = build::layer(p, comp, "Shape", LayerSource::Shape, (comp.width, comp.height), None);
        let mut next = p.next_id;
        let mut ids = Ids(&mut next);
        let star = build::shape_star(&mut ids, true, 5.0, offset, 30.0, 12.0);
        let stroke = build::shape_stroke(&mut ids, [1.0, 1.0, 1.0, 1.0], 6.0);
        let fill = build::shape_fill(&mut ids, [0.9, 0.2, 0.1, 1.0]);
        let g = build::shape_group(&mut ids, "Star", vec![star, stroke, fill]);
        p.next_id = next;
        l.props.sub_mut("contents").unwrap().children.push(g.into());
        l
    }

    fn place(l: &mut Layer, pos: [f64; 2], rot: f64, scale: f64) {
        l.props.prop_mut("transform/position").unwrap().value = Value::Vec3([pos[0], pos[1], 0.0]);
        l.props.prop_mut("transform/rotation").unwrap().value = Value::Scalar(rot);
        l.props.prop_mut("transform/scale").unwrap().value = Value::Vec3([scale, scale, 100.0]);
    }

    /// The rectangle measured before rendering is the source buffer's (text: stroke, blur,
    /// continuous rasterisation; shapes: Continuously Rasterize), at full and half resolution.
    #[test]
    fn drawn_rect_is_the_source_buffers() {
        let (mut p, cid, comp) = setup();
        let mut layers = vec![text(&mut p, &comp, 0.0), text(&mut p, &comp, 5.0), blurred_text(&mut p, &comp), shape(&mut p, &comp, [40.0, -20.0])];
        let mut big = shape(&mut p, &comp, [0.0, 0.0]);
        big.switches.collapse = true;
        place(&mut big, [10.0, 10.0], 30.0, 340.0);
        let mut small = text(&mut p, &comp, 2.0);
        place(&mut small, [10.0, 10.0], 0.0, 13.0);
        layers.extend([big, small]);
        p.comp_mut(cid).unwrap().layers = layers.into();
        let c = p.comp(cid).unwrap();
        for scale in [1.0, 0.5] {
            let r = Renderer::new(&p, &NoFootage, RenderOpts { scale, ..Default::default() });
            let ctx = EvalCtx::new(&p, cid, c, Tick::ZERO);
            for (i, l) in c.layers.iter().enumerate() {
                let b = r.source(&ctx, l).unwrap();
                let k = b.scale;
                let want = [-b.offset[0] / k, -b.offset[1] / k, (b.img.width as f64 - b.offset[0]) / k, (b.img.height as f64 - b.offset[1]) / k];
                let got = r.drawn_rect(&ctx, l).unwrap();
                assert_eq!([got.x0, got.y0, got.x1, got.y1], want, "layer {i} at scale {scale}");
            }
        }
    }

    fn frame(p: &Project, cid: ItemId, cull: bool) -> (Image, Vec<String>) {
        NO_CULL.with(|c| c.set(!cull));
        let prof = std::sync::Mutex::new(vec![]);
        let mut r = Renderer::new(p, &NoFootage, RenderOpts::default());
        r.profile = Some(&prof);
        let img = r.comp_frame(cid, Tick::ZERO);
        NO_CULL.with(|c| c.set(false));
        (img, prof.into_inner().unwrap().into_iter().map(|t| t.layer).collect())
    }

    /// Text and shape layers swept around and across the comp's edges (rotated, scaled, stroked,
    /// blurred, their content away from the anchor): culling skips the ones off the comp and
    /// the frames are identical to drawing every layer.
    #[test]
    fn culled_text_and_shapes_leave_frames_identical() {
        let (mut p, cid, comp) = setup();
        let sid = p.add_item("BG", Label::Red, None, ItemKind::Solid(Solid { color: [0.3, 0.4, 0.5], width: 200, height: 100, pixel_aspect: 1.0 }));
        let bg = build::layer(&mut p, &comp, "BG", LayerSource::Solid { item: sid }, (200, 100), None);
        let mut culled = 0;
        let mut drawn = 0;
        let spots = [-140.0, -70.0, -35.0, -12.0, 0.0, 100.0, 212.0, 235.0, 270.0, 340.0];
        for (n, &x) in spots.iter().enumerate() {
            for &y in &[-90.0, -30.0, -8.0, 50.0, 108.0, 140.0, 200.0] {
                let mut layers: Vec<Layer> = vec![];
                for (j, mut l) in [text(&mut p, &comp, 4.0), blurred_text(&mut p, &comp), shape(&mut p, &comp, [55.0, 0.0]), shape(&mut p, &comp, [0.0, -45.0])].into_iter().enumerate() {
                    place(&mut l, [x, y], 23.0 * (n + j) as f64, 60.0 + 35.0 * j as f64);
                    l.switches.collapse = j % 2 == 0;
                    l.name = format!("{j}");
                    layers.push(l);
                }
                layers.push(bg.clone());
                p.comp_mut(cid).unwrap().layers = layers.into();
                let (a, names) = frame(&p, cid, true);
                let (b, all) = frame(&p, cid, false);
                assert_eq!(all.len(), 5, "every layer drawn without culling");
                assert!(a == b, "at ({x}, {y}): culling changed the frame (drawn {names:?})");
                culled += all.len() - names.len();
                drawn += names.len() - 1;
            }
        }
        assert!(culled > 60 && drawn > 30, "culled {culled}, drawn {drawn}");
    }
}
