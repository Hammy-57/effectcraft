//! Layer cache keys of precomp layers: the nested comp's content at the nested time, so a
//! precomp whose nested comp doesn't change renders once (and an Essential Properties instance
//! doesn't copy the project on a hit).
//!
//! A nested comp's key ([`Renderer::comp_key`]) hashes everything its frame is made of: the
//! comp's settings and every layer (structure memoised per shared comp and layer, see
//! [`crate::cache`]), each animated property's value at the nested time (expressions included,
//! so one reading another comp is keyed by what it reads), which layers are active, the items
//! they show (footage by source time unless it is a still), nested precomps recursively, and the
//! renderer state that reaches nested comps (scale, quality, switches, proxies, depth). The time
//! itself counts when something reads the clock or other times (time effects, Wiggle, paint
//! spans, motion blur, Auto-Orient along a path); a comp where such a reader meets an expression
//! (which may read other comps at those times, out of the key's sight) is not keyed.

use std::hash::{Hash, Hasher};
use std::sync::{Arc, LazyLock, RwLock};

use effectcraft_project::essential::{self, EgKind, Override};
use effectcraft_project::{AutoOrient, Comp, FootageKind, FrameBlend, ItemId, ItemKind, Layer, LayerSource, Layers, Project};
use effectcraft_time::Tick;

use crate::cache::{FootageKey, Memo, ProxyKey, hash_debug, hash_values, hasher, layer_info_arc};
use crate::footage::frame_blend_mode;
use crate::sources::essential_overrides;
use crate::{EvalCtx, RenderOpts, Renderer};

/// Nesting limit for keys (as for rendering, see `Renderer::comp_frame_cpu`).
const MAX_DEPTH: usize = 16;

/// What a comp's key needs that changes only when the comp does (memoised per shared comp).
struct CompInfo {
    /// The comp's settings and every layer, hashed.
    structure: u64,
    /// Layers with keyframes or expressions: their values are hashed per frame.
    animated: Vec<usize>,
    /// Layers showing an item (footage, solid, precomp, model).
    sources: Vec<usize>,
    /// Nested comps (precomp layers' items).
    nested: Vec<ItemId>,
    /// A property has an expression.
    expressions: bool,
    /// A layer reads the clock or other times by itself (time effects, Wiggle, paint, Auto-Orient
    /// along a path).
    other_times: bool,
    /// A layer has its Motion Blur switch on.
    motion_blur: bool,
}

impl CompInfo {
    fn of(c: &Comp) -> CompInfo {
        let mut h = hasher(0x51ed_270b_27d3_8d41);
        let mut settings = c.clone();
        settings.layers = Layers::new();
        hash_debug(&mut h, &settings);
        let mut info = CompInfo { structure: 0, animated: vec![], sources: vec![], nested: vec![], expressions: false, other_times: false, motion_blur: false };
        for (i, l) in c.layers.arcs().iter().enumerate() {
            let li = layer_info_arc(l);
            li.whole.hash(&mut h);
            if li.animated {
                info.animated.push(i);
            }
            if l.source.item().is_some() {
                info.sources.push(i);
            }
            if let LayerSource::Comp { item } = l.source {
                info.nested.push(item);
            }
            info.expressions |= li.expressions;
            info.other_times |= li.time_dependent || li.reads_other_times || li.paint || l.auto_orient == AutoOrient::AlongPath;
            info.motion_blur |= l.switches.motion_blur;
        }
        info.structure = h.finish();
        info
    }
}

static COMPS: LazyLock<RwLock<Memo<Comp, Arc<CompInfo>>>> = LazyLock::new(|| RwLock::new(Memo::new()));

fn comp_info(arc: &Arc<Comp>) -> Arc<CompInfo> {
    if let Some(i) = COMPS.read().ok().and_then(|m| m.get(Arc::as_ptr(arc))) {
        return i;
    }
    let info = Arc::new(CompInfo::of(arc));
    if let Ok(mut m) = COMPS.write() {
        m.put(arc, info.clone());
    }
    info
}

/// The shared handle of comp `id`.
fn comp_arc(project: &Project, id: ItemId) -> Option<&Arc<Comp>> {
    match &project.item(id)?.kind {
        ItemKind::Comp(c) => Some(c),
        _ => None,
    }
}

/// Does comp `id` or a comp nested in it have an expression (`expressions`), or a motion-blurred
/// layer (Motion Blur switch on in a comp with Enable Motion Blur on)?
fn tree_has(project: &Project, id: ItemId, expressions: bool, depth: usize) -> bool {
    if depth > MAX_DEPTH {
        return true;
    }
    let Some(arc) = comp_arc(project, id) else { return false };
    let info = comp_info(arc);
    let here = if expressions { info.expressions } else { arc.enable_motion_blur && info.motion_blur };
    here || info.nested.iter().any(|n| tree_has(project, *n, expressions, depth + 1))
}

/// Does comp `id` change from frame to frame: a layer animates or reads the clock in it or a
/// comp nested in it, or it shows moving footage?
fn tree_varies(project: &Project, id: ItemId, depth: usize) -> bool {
    if depth > MAX_DEPTH {
        return true;
    }
    let Some(arc) = comp_arc(project, id) else { return false };
    let info = comp_info(arc);
    if !info.animated.is_empty() || info.other_times {
        return true;
    }
    let moving = |l: &Layer| match l.source {
        LayerSource::Footage { item } => !matches!(project.item(item).map(|i| &i.kind), Some(ItemKind::Footage(f)) if f.kind == FootageKind::Still),
        _ => false,
    };
    info.sources.iter().any(|&i| arc.layers.get(i).is_some_and(moving)) || info.nested.iter().any(|n| tree_varies(project, *n, depth + 1))
}

impl<'a> Renderer<'a> {
    /// Whether precomp layer `layer`'s pixels change from frame to frame (its nested comps
    /// animate, or its own masks or effects do): its buffer is only reused within a frame (see
    /// [`crate::cache::LayerCache::insert_once`]).
    pub(crate) fn precomp_varies(&self, ctx: &EvalCtx, layer: &Layer) -> bool {
        let LayerSource::Comp { item } = layer.source else { return false };
        crate::cache::content_animated(ctx.comp, layer) || tree_varies(self.project, item, 0)
    }

    /// The key of what precomp layer `layer` shows at the context time (its nested comp as
    /// [`Self::source`] draws it), or `None` when it can't be keyed (see the module docs).
    pub(crate) fn precomp_key(&self, ctx: &EvalCtx, layer: &Layer) -> Option<u64> {
        let LayerSource::Comp { item } = layer.source else { return None };
        let mut h = hasher(0x2545_f491_4f6c_dd1d);
        // Nothing is drawn (the comp contains this one, or the time is outside it).
        if self.project.comp_contains(item, ctx.comp_id) {
            0u8.hash(&mut h);
            return Some(h.finish());
        }
        let lt = ctx.nested_time(layer);
        let nc = self.project.comp(item)?;
        if !nc.covers(lt) {
            1u8.hash(&mut h);
            return Some(h.finish());
        }
        if let Some(pf) = self.proxy_for(item) {
            2u8.hash(&mut h);
            hash_debug(&mut h, &FootageKey(pf));
            lt.0.hash(&mut h);
            return Some(h.finish());
        }
        // Frame blending mixes two nested frames: not keyed.
        if frame_blend_mode(ctx, layer) != FrameBlend::Off {
            return None;
        }
        let (sub, bs) = self.precomp_renderer(layer, item);
        3u8.hash(&mut h);
        bs.to_bits().hash(&mut h);
        let ov = essential_overrides(ctx, layer);
        let key = if ov.is_empty() {
            sub.comp_key(item, lt)?
        } else {
            hash_debug(&mut h, &ov);
            if plain_overrides(self.project, item, nc, &ov) {
                // The instance's values are in `ov`: no copy of the project.
                sub.comp_key(item, lt)?
            } else {
                match essential::with_overrides(self.project, item, &ov) {
                    Some(p) => Renderer { project: &p, ..sub }.comp_key(item, lt)?,
                    None => sub.comp_key(item, lt)?,
                }
            }
        };
        key.hash(&mut h);
        Some(h.finish())
    }

    /// The content key of comp `id` drawn by this renderer at `t` (see the module docs).
    pub(crate) fn comp_key(&self, id: ItemId, t: Tick) -> Option<u64> {
        if self.depth > MAX_DEPTH {
            return None;
        }
        let arc = comp_arc(self.project, id)?;
        let info = comp_info(arc);
        let nc: &Comp = arc;
        // Motion blur evaluates transforms (masks, shapes, text, the layers of collapsed precomps)
        // within the shutter: the time counts only when something in the tree moves.
        let blur = self.opts.motion_blur && tree_has(self.project, id, false, self.depth) && tree_varies(self.project, id, self.depth);
        if (info.other_times || blur) && info.expressions {
            return None;
        }
        let mut h = hasher(0x9ae1_6a3b_2f90_404f);
        info.structure.hash(&mut h);
        id.hash(&mut h);
        hash_debug(&mut h, &self.project.settings);
        hash_debug(&mut h, &RenderOpts { view: None, roi: None, ..self.opts });
        hash_debug(&mut h, &self.inherited);
        self.depth.hash(&mut h);
        self.active_accel().is_some().hash(&mut h);
        if info.other_times || blur {
            t.0.hash(&mut h);
        }
        for l in nc.layers.iter() {
            l.is_active_at(t).hash(&mut h);
        }
        let ctx = self.ctx(id, nc, t);
        for &i in &info.animated {
            let l = nc.layers.get(i)?;
            hash_values(&mut h, &ctx, l, &l.props);
        }
        for &i in &info.sources {
            let l = nc.layers.get(i)?;
            match &l.source {
                LayerSource::Footage { item } | LayerSource::Model { item } => {
                    let it = self.project.item(*item);
                    match it.map(|it| (&it.kind, &it.proxy)) {
                        Some((ItemKind::Footage(f), proxy)) => {
                            hash_debug(&mut h, &FootageKey(f));
                            hash_debug(&mut h, &ProxyKey(proxy));
                            let used = self.proxy_for(*item);
                            used.is_some().hash(&mut h);
                            if used.unwrap_or(f).kind != FootageKind::Still {
                                ctx.source_time(l).0.hash(&mut h);
                            }
                        }
                        other => hash_debug(&mut h, &other.map(|o| o.0)),
                    }
                }
                LayerSource::Solid { item } => hash_debug(&mut h, &self.project.item(*item).map(|i| &i.kind)),
                LayerSource::Comp { .. } => self.precomp_key(&ctx, l)?.hash(&mut h),
                _ => {}
            }
        }
        Some(h.finish())
    }
}

/// Instance overrides that the original project plus the override values determine: no Media
/// replacement (another item) and no expression in the nested comps (one could read an
/// overridden property).
fn plain_overrides(project: &Project, item: ItemId, nc: &Comp, ov: &[Override]) -> bool {
    let media = nc.essential.as_ref().is_some_and(|eg| ov.iter().any(|o| eg.resolve(o.control).is_some_and(|c| matches!(c.kind, EgKind::Media { .. }))));
    !media && !tree_has(project, item, true, 0)
}
