//! Processed layer buffers: source → masks → effects → layer styles, served from and
//! stored in the layer cache.

use crate::{EvalCtx, LayerTiming, Renderer, cache, color, masks, source_size, styles};
use effectcraft_color::BlendMode;
use effectcraft_effects::Buf;
use effectcraft_project::{Layer, LayerSource};
use std::sync::Arc;

impl<'a> Renderer<'a> {
    /// Source → masks, clamped/quantised to the project depth.
    fn masked_source(&self, ctx: &EvalCtx, layer: &Layer) -> Option<Buf> {
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
                return Some((b, key));
            }
        }
        let buf = self.masked_source(ctx, layer)?;
        let buf = Arc::new(self.apply_effects_timed(ctx, layer, buf, false, usize::MAX, timing.map(|t| &mut t.effects)));
        if let (Some(c), Some(k)) = (self.cache, key) {
            c.insert(k, buf.clone());
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
        let key = self.cache.and_then(|_| cache::input_key_src(ctx, layer, self.raster_scale(ctx, layer), self.opts.draft, self.mb_on(ctx, layer), effects, src));
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
        self.styled(ctx, layer, timing).map(|st| self.styled_to_blend(st))
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
