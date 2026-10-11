//! Processed layer buffers: source → masks → effects → layer styles, served from and
//! stored in the layer cache.

use crate::{EvalCtx, LayerTiming, Renderer, cache, color, masks, source_size, styles};
use effectcraft_color::BlendMode;
use effectcraft_effects::Buf;
use effectcraft_keyframe::Value;
use effectcraft_project::{Comp, Layer, LayerSource, Node, PropGroup};
use std::sync::Arc;

impl<'a> Renderer<'a> {
    /// Source → masks, clamped/quantised to the project depth.
    fn masked_source(&self, ctx: &EvalCtx, layer: &Layer) -> Option<Buf> {
        let _profile = crate::profile::span("layer_raster");
        let mut buf = match self.content_times(ctx, layer) {
            Some(times) => self.blurred_source(ctx, layer, &times)?,
            None => self.source(ctx, layer)?,
        };
        // A solid is filled with a quantised colour: only masks can take it off the grid.
        let solid = matches!(layer.source, LayerSource::Solid { .. });
        if masks::apply(ctx, layer, &mut buf, self.mask_blur(ctx, layer)) || !solid {
            self.pipe.quantize(&mut buf.img);
        }
        Some(buf)
    }

    /// Fully processed layer buffer (source → masks → effects → layer styles, flattened).
    /// Served from the layer cache when the layer's content key matches.
    pub fn layer_buf(&self, ctx: &EvalCtx, layer: &Layer) -> Option<Arc<Buf>> {
        self.layer_buf_keyed(ctx, layer).map(|(b, _)| b)
    }

    /// The pixels a layer lends as a track matte: its content (source, masks, effects) before
    /// Layer Styles and their blending options. After Effects mattes through a shape whose Fill
    /// Opacity is 0 as through the shape (SceneYard "Apple Maps Route").
    pub fn matte_buf(&self, ctx: &EvalCtx, layer: &Layer) -> Option<Arc<Buf>> {
        self.styled(ctx, layer, None).map(|st| st.content)
    }

    /// [`Self::layer_buf`] in the blending space (linear with Blend Colors Using 1.0 Gamma).
    pub fn blend_layer_buf(&self, ctx: &EvalCtx, layer: &Layer) -> Option<Arc<Buf>> {
        let (b, k) = self.layer_buf_keyed(ctx, layer)?;
        Some(self.to_blend(b, k))
    }

    /// A buffer converted to the blending space (cached under a key derived from `key`).
    pub(crate) fn to_blend(&self, b: Arc<Buf>, key: Option<u64>) -> Arc<Buf> {
        let Some(c) = self.pipe.to_blend() else { return b };
        let key = key.map(|k| cache::derive(k, 0x1ea7));
        if let (Some(cache), Some(k)) = (self.cache, key)
            && let Some(x) = cache.get(k)
        {
            return x;
        }
        let mut nb = (*b).clone();
        color::convert(&mut nb.img, &c);
        let nb = Arc::new(nb);
        if let (Some(cache), Some(k)) = (self.cache, key) {
            cache.insert(k, nb.clone());
        }
        nb
    }

    fn layer_buf_keyed(&self, ctx: &EvalCtx, layer: &Layer) -> Option<(Arc<Buf>, Option<u64>)> {
        let st = self.styled(ctx, layer, None)?;
        if st.passes.is_empty() {
            return Some((st.body, st.key));
        }
        let key = st.key.map(|k| cache::derive(k, 0xf1a7));
        if let (Some(c), Some(k)) = (self.cache, key)
            && let Some(b) = c.get(k)
        {
            return Some((b, key));
        }
        let flat = Arc::new(styles::Styled { passes: st.passes.iter().map(|(b, m)| ((**b).clone(), *m)).collect(), body: (*st.body).clone() }.flatten());
        if let (Some(c), Some(k)) = (self.cache, key) {
            c.insert(k, flat.clone());
        }
        Some((flat, key))
    }

    /// A layer's source pixels at the context time, before masks, effects and the transform
    /// (what the motion tracker analyses, like After Effects' Layer panel). `None` for layers
    /// without pixels.
    pub fn layer_source(&self, ctx: &EvalCtx, layer: &Layer) -> Option<Buf> {
        self.source(ctx, layer)
    }

    /// Layer pixels after source → masks → effects (no layer styles).
    pub fn content_buf(&self, ctx: &EvalCtx, layer: &Layer) -> Option<Arc<Buf>> {
        self.content_buf_timed(ctx, layer, None).map(|(b, _)| b)
    }

    /// The layer cache key of a layer's content (`None` without a cache or for uncached layers).
    fn content_cache_key(&self, ctx: &EvalCtx, layer: &Layer) -> Option<u64> {
        self.content_cache_key_noted(ctx, layer).0
    }

    /// [`Self::content_cache_key`], and for a precomp layer that was keyed, its id for
    /// [`cache::LayerCache::precomp_note`] (an animated precomp is keyed only now and then, see
    /// [`cache::LayerCache::precomp_try`]).
    fn content_cache_key_noted(&self, ctx: &EvalCtx, layer: &Layer) -> (Option<u64>, Option<u64>) {
        let Some(c) = self.cache else { return (None, None) };
        let (src, noted) = match layer.source {
            LayerSource::Comp { .. } => {
                let id = cache::derive(cache::derive(ctx.comp_id.0, layer.id.0), self.depth as u64);
                if !c.precomp_try(id) {
                    return (None, None);
                }
                (self.precomp_key(ctx, layer), Some(id))
            }
            // Which proxy the footage shows.
            LayerSource::Footage { item } => (Some(self.proxy_for(item).is_some() as u64), None),
            _ => (None, None),
        };
        let key = cache::layer_key_src(ctx, layer, self.raster_scale(ctx, layer), self.opts.draft, self.mb_on(ctx, layer), src);
        (self.content_key(ctx, layer, key), noted)
    }

    fn content_buf_timed(&self, ctx: &EvalCtx, layer: &Layer, mut timing: Option<&mut LayerTiming>) -> Option<(Arc<Buf>, Option<u64>)> {
        let (key, noted) = self.content_cache_key_noted(ctx, layer);
        // An animated precomp's buffer isn't kept for other uses in its frame (see
        // [`cache::LayerCache::insert_once`]): no use waiting for another thread's render of it.
        let once = key.is_some() && noted.is_some() && self.precomp_varies(ctx, layer);
        let mut _flight = None;
        if let Some(c) = self.cache {
            let hit = match key {
                Some(k) if once => c.get(k),
                Some(k) => c.get(k).or_else(|| match c.claim(k) {
                    cache::Claim::Ready(b) => Some(b),
                    cache::Claim::Owner(g) => {
                        _flight = Some(g);
                        None
                    }
                    cache::Claim::Render => None,
                }),
                None => None,
            };
            if let Some(id) = noted {
                c.precomp_note(id, hit.is_some());
            }
            if let Some(b) = hit {
                if let Some(t) = timing.as_deref_mut() {
                    t.cached = true;
                }
                return Some((b, key));
            }
        }
        let buf = self.masked_source(ctx, layer)?;
        let (buf, kept) = self.apply_effects_kept(ctx, layer, buf, false, usize::MAX, timing.map(|t| &mut t.effects));
        let buf = Arc::new(buf);
        // The accelerator composites this buffer from the effects' result on the device (no
        // second upload), unless it blends a converted copy (linear blending).
        if let Some(k) = kept
            && self.pipe.to_blend().is_none()
        {
            k.adopt(&buf);
        }
        if let (Some(c), Some(k)) = (self.cache, key) {
            if once {
                c.insert_once(k, buf.clone());
            } else {
                c.insert(k, buf.clone());
            }
        }
        // (`_flight`, dropped here, wakes the threads waiting for this buffer.)
        Some((buf, key))
    }

    /// A layer's input at the context time: source → masks → its first `effects` effects
    /// (what Time effects read at neighbouring times), in effect space (see [`effect_bounds`]):
    /// what effect `effects` sees. Served from / stored in the layer cache under its own key,
    /// so scrubbing reuses frames rendered for earlier output frames.
    pub fn layer_input(&self, ctx: &EvalCtx, layer: &Layer, effects: usize) -> Option<Arc<Buf>> {
        let src = match layer.source {
            LayerSource::Comp { .. } if self.cache.is_some() => self.precomp_key(ctx, layer),
            LayerSource::Footage { item } => Some(self.proxy_for(item).is_some() as u64),
            _ => None,
        };
        let key =
            self.cache.and_then(|_| cache::input_key_src(ctx, layer, self.raster_scale(ctx, layer), self.opts.draft, self.mb_on(ctx, layer), effects, src));
        let key = self.content_key(ctx, layer, key);
        if let (Some(c), Some(k)) = (self.cache, key)
            && let Some(b) = c.get(k)
        {
            return Some(b);
        }
        let mut buf = self.masked_source(ctx, layer)?;
        let origin = ctx.effect_bounds(layer).1;
        buf.rebase(origin);
        if effects > 0 {
            buf = self.effect_space_stack(ctx, layer, buf, false, effects, None, origin);
        }
        let buf = Arc::new(buf);
        if let (Some(c), Some(k)) = (self.cache, key) {
            c.insert(k, buf.clone());
        }
        Some(buf)
    }

    /// The layer with its styles: exterior passes and body (cached per pass).
    pub(crate) fn styled(&self, ctx: &EvalCtx, layer: &Layer, mut timing: Option<&mut LayerTiming>) -> Option<StyledLayer> {
        let (content, ckey) = self.content_buf_timed(ctx, layer, timing.as_deref_mut())?;
        if !styles::active(ctx, layer) {
            return Some(StyledLayer { passes: vec![], body: content.clone(), content, key: ckey, ckey, plain: true });
        }
        let modes = styles::pass_modes(ctx, layer);
        let key = ckey.map(|k| cache::styles_key(ctx, layer, k));
        if let (Some(c), Some(k)) = (self.cache, key)
            && let Some(body) = c.get(k)
        {
            let passes: Option<Vec<_>> = modes.iter().enumerate().map(|(i, m)| c.get(cache::derive(k, i as u64 + 1)).map(|b| (b, *m))).collect();
            if let Some(passes) = passes {
                return Some(StyledLayer { passes, body, content, key, ckey, plain: false });
            }
        }
        let t0 = web_time::Instant::now();
        let size = source_size(self.project, layer);
        let layer_size = if size.0 == 0 { [ctx.comp.width as f64, ctx.comp.height as f64] } else { [size.0 as f64, size.1 as f64] };
        let st = styles::render(ctx, layer, &content, layer_size);
        if let Some(t) = timing {
            t.cached = false;
            t.effects.push(("layerStyles".into(), t0.elapsed().as_secs_f64() * 1e3));
        }
        let body = Arc::new(st.body);
        let passes: Vec<_> = st.passes.into_iter().map(|(b, m)| (Arc::new(b), m)).collect();
        if let (Some(c), Some(k)) = (self.cache, key) {
            c.insert(k, body.clone());
            for (i, (b, _)) in passes.iter().enumerate() {
                c.insert(cache::derive(k, i as u64 + 1), b.clone());
            }
        }
        Some(StyledLayer { passes, body, content, key, ckey, plain: false })
    }

    /// [`Self::styled`] in the blending space ([`Self::styled_to_blend`]). With linear blending,
    /// the blending-space copies are looked up first: on a hit the working-space buffers they
    /// were converted from aren't touched, so they go cold in the layer cache (packed or evicted
    /// under pressure) instead of being kept hot next to their converted copies.
    pub(crate) fn styled_blend(&self, ctx: &EvalCtx, layer: &Layer, mut timing: Option<&mut LayerTiming>) -> Option<StyledLayer> {
        if let Some(st) = self.styled_blend_cached(ctx, layer) {
            if let Some(t) = timing.as_deref_mut() {
                t.cached = true;
            }
            return Some(st);
        }
        if self.blend_only(ctx, layer) {
            return self.plain_blend(ctx, layer, timing);
        }
        self.styled(ctx, layer, timing).map(|st| self.styled_to_blend(st))
    }

    /// With linear blending, is `layer` a plain layer (no Layer Styles) whose working-space
    /// pixels nothing else reads? Then only its blending-space copy is worth caching.
    fn blend_only(&self, ctx: &EvalCtx, layer: &Layer) -> bool {
        self.cache.is_some() && self.pipe.to_blend().is_some() && !styles::active(ctx, layer) && !working_read(ctx.comp, layer)
    }

    /// [`Self::styled_blend`] of a [`Self::blend_only`] layer: its content is converted in place
    /// and cached in the blending space alone (no working-space copy cloned and kept next to
    /// it). The same pixels as [`Self::content_buf_timed`] then [`Self::to_blend`].
    fn plain_blend(&self, ctx: &EvalCtx, layer: &Layer, mut timing: Option<&mut LayerTiming>) -> Option<StyledLayer> {
        let (ckey, noted) = self.content_cache_key_noted(ctx, layer);
        let key = ckey.map(|k| cache::derive(k, 0x1ea7));
        let mut _flight = None;
        if let Some(c) = self.cache {
            let hit = match key {
                Some(k) => c.get(k).or_else(|| match c.claim(k) {
                    cache::Claim::Ready(b) => Some(b),
                    cache::Claim::Owner(g) => {
                        _flight = Some(g);
                        None
                    }
                    cache::Claim::Render => None,
                }),
                None => None,
            };
            if let Some(id) = noted {
                c.precomp_note(id, hit.is_some());
            }
            if let Some(b) = hit {
                if let Some(t) = timing.as_deref_mut() {
                    t.cached = true;
                }
                return Some(StyledLayer { passes: vec![], content: b.clone(), body: b, key: ckey, ckey, plain: true });
            }
        }
        let buf = self.masked_source(ctx, layer)?;
        let mut buf = self.apply_effects_timed(ctx, layer, buf, false, usize::MAX, timing.map(|t| &mut t.effects));
        if let Some(c) = self.pipe.to_blend() {
            color::convert(&mut buf.img, &c);
        }
        let buf = Arc::new(buf);
        if let (Some(c), Some(k)) = (self.cache, key) {
            c.insert(k, buf.clone());
        }
        // (`_flight`, dropped here, wakes the threads waiting for this buffer.)
        Some(StyledLayer { passes: vec![], content: buf.clone(), body: buf, key: ckey, ckey, plain: true })
    }

    fn styled_blend_cached(&self, ctx: &EvalCtx, layer: &Layer) -> Option<StyledLayer> {
        let c = self.cache?;
        self.pipe.to_blend()?;
        let ckey = self.content_cache_key(ctx, layer)?;
        let blend = |k: u64| c.peek(cache::derive(k, 0x1ea7));
        if !styles::active(ctx, layer) {
            let body = blend(ckey)?;
            return Some(StyledLayer { passes: vec![], content: body.clone(), body, key: Some(ckey), ckey: Some(ckey), plain: true });
        }
        let key = cache::styles_key(ctx, layer, ckey);
        let body = blend(key)?;
        let content = blend(ckey)?;
        let passes = styles::pass_modes(ctx, layer)
            .into_iter()
            .enumerate()
            .map(|(i, m)| blend(cache::derive(key, i as u64 + 1)).map(|b| (b, m)))
            .collect::<Option<Vec<_>>>()?;
        Some(StyledLayer { passes, body, content, key: Some(key), ckey: Some(ckey), plain: false })
    }

    /// The styled layer's buffers in the blending space.
    pub(crate) fn styled_to_blend(&self, st: StyledLayer) -> StyledLayer {
        if self.pipe.to_blend().is_none() {
            return st;
        }
        let body = self.to_blend(st.body, st.key);
        let content = if st.plain { body.clone() } else { self.to_blend(st.content, st.ckey) };
        let passes = st.passes.into_iter().enumerate().map(|(i, (b, m))| (self.to_blend(b, st.key.map(|k| cache::derive(k, i as u64 + 1))), m)).collect();
        StyledLayer { passes, body, content, ..st }
    }
}

/// Could anything in `layer`'s comp read its working-space pixels (its content buffer) other
/// than compositing it: a track matte naming it, an effect's layer parameter (Displacement Map,
/// Set Matte…) or an expression's `sampleImage`? Errs towards yes: a wrong yes only keeps a
/// copy in the layer cache, a wrong no only re-renders the layer for that reader.
fn working_read(comp: &Comp, layer: &Layer) -> bool {
    comp.layers.iter().any(|l| {
        l.track_matte.as_ref().is_some_and(|m| m.layer == layer.id)
            || l.effects().is_some_and(names_a_layer)
            || (cache::layer_info(comp, l).expressions && samples_pixels(&l.props))
    })
}

/// Does a property of `g` take a layer (whatever its value)?
fn names_a_layer(g: &PropGroup) -> bool {
    g.children.iter().any(|c| match c {
        Node::Prop(p) => matches!(p.value, Value::Layer(_)),
        Node::Group(sub) => names_a_layer(sub),
    })
}

/// Does an expression in `g` read rendered pixels (`sampleImage`)?
fn samples_pixels(g: &PropGroup) -> bool {
    g.children.iter().any(|c| match c {
        Node::Prop(p) => p.expr.as_ref().is_some_and(|e| e.text.contains("sampleImage")),
        Node::Group(sub) => samples_pixels(sub),
    })
}

/// A layer ready to composite (see [`Renderer::styled_layer`]).
pub struct StyledLayer {
    /// Exterior style passes (Drop Shadow, Outer Glow) with their blend modes.
    pub passes: Vec<(Arc<Buf>, BlendMode)>,
    pub body: Arc<Buf>,
    /// Pre-style pixels (knockout shape).
    pub content: Arc<Buf>,
    /// Styled cache key (the content key when plain).
    key: Option<u64>,
    /// Content cache key.
    ckey: Option<u64>,
    /// No layer styles: composite `body` the usual way.
    pub plain: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{LayerCache, NoFootage, RenderOpts};
    use effectcraft_color::Label;
    use effectcraft_project::build::{self, Ids};
    use effectcraft_project::{Expression, ItemId, ItemKind, MatteKind, Project, Property, Solid, TrackMatte};
    use effectcraft_time::{FrameRate, Tick};

    fn solid(p: &mut Project, c: &Comp, color: [f32; 3], w: u32) -> Layer {
        let sid = p.add_item("S", Label::Red, None, ItemKind::Solid(Solid { color, width: w, height: 20, pixel_aspect: 1.0 }));
        build::layer(p, c, "S", LayerSource::Solid { item: sid }, (w, 20), None)
    }

    fn blur(p: &mut Project, l: &mut Layer, amount: f64) {
        let Some(spec) = effectcraft_effects::find("ec.blur.gaussian") else { panic!("gaussian") };
        let mut next = p.next_id;
        let mut g = effectcraft_effects::instantiate(spec, &mut Ids(&mut next), spec.name, [40.0, 20.0]);
        p.next_id = next;
        let Some(b) = g.prop_mut("blurriness") else { panic!("blurriness") };
        b.value = Value::Scalar(amount);
        let Some(fx) = l.props.sub_mut("effects") else { panic!("effects") };
        fx.children.push(g.into());
    }

    /// A linear-blending comp: a blurred solid screened over a solid, the top one optionally
    /// the bottom one's track matte.
    fn scene(matte: bool) -> (Project, ItemId) {
        let mut p = Project::default();
        p.settings.blend_linear = true;
        let comp = Comp::new(40, 20, FrameRate::FPS_30, Tick::from_seconds_f64(2.0));
        let cid = p.add_item("Comp", Label::Sandstone, None, ItemKind::Comp(comp.clone().into()));
        let bottom = solid(&mut p, &comp, [0.1, 0.5, 0.9], 40);
        let mut top = solid(&mut p, &comp, [0.8, 0.2, 0.1], 24);
        blur(&mut p, &mut top, 3.0);
        top.blend_mode = BlendMode::Screen;
        let mut bottom = bottom;
        if matte {
            bottom.track_matte = Some(TrackMatte { layer: top.id, kind: MatteKind::Alpha });
        }
        let Some(c) = p.comp_mut(cid) else { panic!("comp") };
        c.layers.insert(0, bottom);
        c.layers.insert(0, top);
        (p, cid)
    }

    /// With linear blending, layers nothing else reads are cached in the blending space alone
    /// (one entry per layer, not two) and the frame is bit-identical to an uncached render,
    /// which converts a working-space copy.
    #[test]
    fn linear_blend_caches_unread_layers_once() {
        let (p, cid) = scene(false);
        let cache = LayerCache::default();
        let frame = || {
            let mut r = Renderer::new(&p, &NoFootage, RenderOpts::default());
            r.cache = Some(&cache);
            r.comp_frame(cid, Tick::ZERO)
        };
        let a = frame();
        assert_eq!(cache.stats().entries, 2, "one blending-space copy per layer");
        let hits = cache.stats().hits;
        let b = frame();
        assert_eq!(cache.stats().hits, hits + 2, "one hit per layer");
        assert_eq!(cache.stats().entries, 2);
        let uncached = Renderer::new(&p, &NoFootage, RenderOpts::default()).comp_frame(cid, Tick::ZERO);
        let bits = |i: &effectcraft_raster::Image| i.data.iter().flat_map(|px| px.map(f32::to_bits)).collect::<Vec<_>>();
        assert_eq!(bits(&a), bits(&uncached));
        assert_eq!(bits(&b), bits(&uncached));
    }

    /// A layer another layer reads as its track matte keeps its working-space copy (the matte
    /// reads it); pixels are unchanged either way.
    #[test]
    fn linear_blend_keeps_working_copies_that_are_read() {
        let (p, cid) = scene(true);
        let cache = LayerCache::default();
        let mut r = Renderer::new(&p, &NoFootage, RenderOpts::default());
        r.cache = Some(&cache);
        let a = r.comp_frame(cid, Tick::ZERO);
        // The matte (top) layer: working and blending copies; the matted layer: blending only.
        assert_eq!(cache.stats().entries, 3);
        let uncached = Renderer::new(&p, &NoFootage, RenderOpts::default()).comp_frame(cid, Tick::ZERO);
        assert_eq!(a.data, uncached.data);
    }

    /// Layer parameters and `sampleImage` expressions count as readers.
    #[test]
    fn working_readers() {
        let (mut p, cid) = scene(false);
        let read = |p: &Project| {
            let Some(c) = p.comp(cid) else { panic!("comp") };
            c.layers.iter().map(|l| working_read(c, l)).collect::<Vec<_>>()
        };
        assert_eq!(read(&p), [false, false]);
        let Some(c) = p.comp_mut(cid) else { panic!("comp") };
        let Some(pos) = c.layers.get_mut(1).and_then(|l| l.props.prop_mut("transform/position")) else { panic!("position") };
        pos.expr = Some(Expression { text: "thisComp.layer(1).sampleImage([10, 10])".into(), enabled: true });
        assert_eq!(read(&p), [true, true]);
        let mut fx = PropGroup::new(1, "fx", "Fx");
        assert!(!names_a_layer(&fx));
        let mut sub = PropGroup::new(2, "sub", "Sub");
        sub.children.push(Node::Prop(Property::new(3, "layer", "Layer", Value::Layer(None))));
        fx.children.push(Node::Group(sub));
        assert!(names_a_layer(&fx));
    }
}
