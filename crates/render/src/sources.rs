//! Layer sources: solids, precomps, footage, text and shapes rasterised in layer space,
//! and their content bounds.

use crate::footage::frame_blend_mode;
use crate::{EvalCtx, Image, RenderOpts, Renderer, blend_frames, color, frame_position, is_vector_footage, shapes, source_size, text};
use effectcraft_effects::Buf;
use effectcraft_geom::vec2;
use effectcraft_project::{FrameBlend, ItemKind, Layer, LayerSource};

/// The Essential Properties overrides of a precomp layer, evaluated at the context time.
pub fn essential_overrides(ctx: &EvalCtx, layer: &Layer) -> Vec<effectcraft_project::essential::Override> {
    use effectcraft_project::essential;
    let over = essential::overridden(layer);
    let Some(g) = essential::group(layer).filter(|_| !over.is_empty()) else { return vec![] };
    let mut out = vec![];
    g.walk("", &mut |_, p| {
        if over.contains(&p.uid)
            && let Some(control) = essential::control_of(&p.match_id)
        {
            out.push(essential::Override { control, value: ctx.value(layer, p) });
        }
    });
    out
}

impl<'a> Renderer<'a> {
    /// Rasterisation scale of a layer's source: the output scale, or — for text layers, which are
    /// always continuously rasterised, and Continuously Rasterize shape layers and vector footage —
    /// the output scale times the layer's on-screen scale (quarter-octave steps, rounded up, so
    /// vector content is drawn at its final size and stays sharp when scaled up).
    pub fn raster_scale(&self, ctx: &EvalCtx, layer: &Layer) -> f64 {
        let s = self.opts.scale;
        let continuous = match layer.source {
            LayerSource::Text => true,
            LayerSource::Shape => layer.switches.collapse,
            LayerSource::Footage { item } => {
                layer.switches.collapse && matches!(self.project.item(item).map(|i| &i.kind), Some(ItemKind::Footage(f)) if is_vector_footage(f))
            }
            _ => false,
        };
        if !continuous {
            return s;
        }
        let (l2c, _) = ctx.layer_to_comp(layer);
        let m = self.outer.map_or(l2c, |o| o * l2c);
        let a = layer.transform().map(|tr| ctx.v3(layer, tr, "anchor", [0.0; 3])).unwrap_or([0.0; 3]);
        let p0 = m.apply(vec2(a[0], a[1]));
        let px = m.apply(vec2(a[0] + 1.0, a[1]));
        let py = m.apply(vec2(a[0], a[1] + 1.0));
        let k = ((px.x - p0.x).hypot(px.y - p0.y)).max((py.x - p0.x).hypot(py.y - p0.y));
        if !k.is_finite() || k <= 1e-6 {
            return s;
        }
        // The step is rounded up past rounding noise only: an unscaled layer whose position isn't
        // a whole number maps the unit vectors to lengths a hair over 1, which must stay 1 (#390:
        // rounded up to 2^¼, unscaled text was drawn larger and resampled, softer).
        let mut k = 2f64.powf((k.log2() * 4.0 - 1e-6).ceil() / 4.0).clamp(1.0 / 16.0, 64.0);
        // Keep the buffer within about 16 million pixels.
        if let Some(b) = content_bounds(ctx, layer) {
            let area = ((b[2] - b[0]) * (b[3] - b[1])).max(1.0) * s * s;
            k = k.min((16.0e6 / area).sqrt().max(1.0));
        }
        s * k
    }

    /// The renderer drawing precomp layer `layer`'s nested comp `item`, and the scale of the
    /// buffer it draws. Preserve resolution when nested: drawn at full size even when this comp
    /// renders smaller (the buffer's scale places it).
    pub(crate) fn precomp_renderer(&self, layer: &Layer, item: effectcraft_project::ItemId) -> (Renderer<'a>, f64) {
        let s = self.opts.scale;
        let full = s < 1.0 && self.project.comp(item).is_some_and(|nc| nc.preserve_resolution);
        let bs = if full { 1.0 } else { s };
        let base = self.nested_through(layer);
        (Renderer { opts: RenderOpts { scale: bs, ..base.opts }, ..base }, bs)
    }

    /// Source pixels of a layer in layer space (before masks/effects).
    pub(crate) fn source(&self, ctx: &EvalCtx, layer: &Layer) -> Option<Buf> {
        let s = self.opts.scale;
        match &layer.source {
            LayerSource::Solid { item } => {
                let ItemKind::Solid(sol) = &self.project.item(*item)?.kind else { return None };
                let w = ((sol.width as f64 * s).ceil() as u32).max(1);
                let h = ((sol.height as f64 * s).ceil() as u32).max(1);
                if layer.switches.adjustment {
                    return Some(Buf { img: Image::filled(w, h, [1.0; 4]), offset: [0.0; 2], scale: s });
                }
                let mut c = sol.color;
                if let Some(conv) = self.pipe.authored_in() {
                    c = conv.apply(c);
                }
                let px = self.pipe.quantize_px([c[0], c[1], c[2], 1.0]);
                Some(Buf { img: Image::filled(w, h, px), offset: [0.0; 2], scale: s })
            }
            LayerSource::Comp { item } => {
                if self.project.comp_contains(*item, ctx.comp_id) {
                    return None;
                }
                // A composition proxy (a rendered still or movie) stands in for the comp.
                // Nothing before the nested comp starts or after it ends.
                let lt = ctx.nested_time(layer);
                if !self.project.comp(*item).is_some_and(|nc| nc.covers(lt)) {
                    return None;
                }
                if let Some(pf) = self.proxy_for(*item)
                    && let Some(nc) = self.project.comp(*item)
                {
                    let img = self.footage.frame(*item, pf, lt)?;
                    return Some(self.footage_buf(&img, pf, nc.width, nc.height));
                }
                // Essential Properties overrides render the nested comp with this instance's
                // values.
                let ov = essential_overrides(ctx, layer);
                let tmp = if ov.is_empty() { None } else { effectcraft_project::essential::with_overrides(self.project, *item, &ov) };
                let (base, bs) = self.precomp_renderer(layer, *item);
                let sub = match &tmp {
                    // A source cache was made without this instance's property overrides.
                    Some(p) => Renderer { project: p, freeze: None, ..base },
                    None => base,
                };
                // The nested frames share their layers' transforms (parents evaluated once).
                let frame = |t| crate::eval::with_transform_memo(sub.project, || sub.comp_frame(*item, t));
                // Frame blending between the nested comp's frames (time-stretched/remapped).
                let mode = frame_blend_mode(ctx, layer);
                if mode != FrameBlend::Off
                    && let Some(nc) = self.project.comp(*item)
                {
                    let (i, w) = frame_position(lt, nc.frame_rate);
                    if w > 1e-6 {
                        let a = frame(nc.frame_rate.tick_of(i));
                        let b = frame(nc.frame_rate.tick_of(i + 1));
                        return Some(Buf { img: blend_frames(mode, &a, &b, w as f32), offset: [0.0; 2], scale: bs });
                    }
                }
                Some(Buf { img: frame(lt), offset: [0.0; 2], scale: bs })
            }
            LayerSource::Footage { item } => {
                let it = self.project.item(*item)?;
                let ItemKind::Footage(f) = &it.kind else { return None };
                if !f.has_video {
                    return None;
                }
                // Continuously rasterised vector footage: drawn at the on-screen scale.
                if layer.switches.collapse && is_vector_footage(f) {
                    let k = self.raster_scale(ctx, layer);
                    if let Some(img) = self.footage.vector_frame(*item, f, k) {
                        let mut buf = Buf { img: (*img).clone(), offset: [0.0; 2], scale: k };
                        if let Some(c) = self.pipe.media_in(f) {
                            color::convert(&mut buf.img, &c);
                        }
                        return Some(buf);
                    }
                }
                let lt = ctx.source_time(layer);
                // The proxy is decoded instead, at its own size, and fills the footage's frame.
                let pf = self.proxy_for(*item).unwrap_or(f);
                let img = self.footage_frame(ctx, layer, *item, pf, lt)?;
                Some(self.footage_buf(&img, pf, f.width, f.height))
            }
            LayerSource::Text => Some(self.authored(text::render(ctx, layer, self.raster_scale(ctx, layer)))),
            LayerSource::Shape => layer.props.sub("contents").map(|c| self.authored(shapes::render(ctx, layer, c, self.raster_scale(ctx, layer)))),
            _ => None,
        }
    }

    /// Authored colours into the working space (linear working spaces).
    fn authored(&self, mut buf: Buf) -> Buf {
        if let Some(c) = self.pipe.authored_in() {
            color::convert(&mut buf.img, &c);
        }
        buf
    }
}

/// Layer-space bounds `[x0, y0, x1, y1]` of a layer's content at the context time (source size
/// for solids/footage/precomps, glyph bounds for text, painted bounds for shapes). Used for viewer
/// handles and hit testing.
pub fn content_bounds(ctx: &EvalCtx, layer: &effectcraft_project::Layer) -> Option<[f64; 4]> {
    match &layer.source {
        LayerSource::Text => {
            let glyphs = text::glyph_paths(ctx, layer);
            let paths: Vec<_> = glyphs.into_iter().map(|g| g.0).collect();
            effectcraft_path::bounds(&paths).map(|r| [r.x0, r.y0, r.x1, r.y1])
        }
        LayerSource::Shape => layer.props.sub("contents").and_then(|c| shapes::content_bounds(ctx, layer, c)).map(|r| [r.x0, r.y0, r.x1, r.y1]),
        LayerSource::Camera | LayerSource::Light { .. } => None,
        _ => {
            let (w, h) = source_size(ctx.project, layer);
            (w > 0 && h > 0).then_some([0.0, 0.0, w as f64, h as f64])
        }
    }
}

/// A mask/shape path as a kurbo path (for drawing overlays).
pub fn kurbo_path(sp: &effectcraft_keyframe::ShapePath) -> kurbo::BezPath {
    effectcraft_path::to_kurbo(sp)
}
