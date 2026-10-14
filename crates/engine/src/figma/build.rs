//! A [`map::Plan`] → project items: rendered images imported as footage, then every
//! composition and layer in one edit, folded with the imports into one undo step ("Import from
//! Figma"). A failure rolls the whole import back.

use std::sync::Arc;

use effectcraft_color::Label;
use effectcraft_keyframe::Value as KV;
use effectcraft_project::build::{self, Ids};
use effectcraft_project::{Comp, ItemId, ItemKind, Layer, LayerId, LayerSource, MaskMode, Project, PropGroup, TrackMatte};
use effectcraft_time::{FrameRate, Tick};
use serde_json::{Value, json};

use super::map::{Item, PKind, PLayer, Plan, Shadow};
use super::{Source, fetch};
use crate::{EngineError, Result, Session};

pub struct Settings {
    pub origin: &'static str,
    pub undo_label: &'static str,
    pub frame_rate: f64,
    pub duration: f64,
    /// Import into this composition instead of new ones.
    pub into: Option<ItemId>,
}

fn set(g: &mut PropGroup, k: &str, v: KV) {
    if let Some(p) = g.get_mut(k) {
        p.value = v;
    }
}

/// Shape contents → property groups.
fn items(ids: &mut Ids, items: &[Item]) -> Vec<PropGroup> {
    let mut paths = 0;
    items
        .iter()
        .filter_map(|it| {
            Some(match it {
                Item::Group { name, items: inner } => {
                    let v = self::items(ids, inner);
                    build::shape_group(ids, name, v)
                }
                Item::Rect { size, position, roundness } => build::shape_rect(ids, *size, *position, *roundness),
                Item::Ellipse { size, position } => build::shape_ellipse(ids, *size, *position),
                Item::Star { star, points, position, outer, inner } => build::shape_star(ids, *star, *points, *position, *outer, *inner),
                Item::Path(sp) => {
                    paths += 1;
                    let mut g = build::shape_path(ids, sp.clone());
                    g.name = format!("Path {paths}");
                    g
                }
                Item::Fill { color, opacity, even_odd } => {
                    let mut g = build::shape_fill(ids, [color[0], color[1], color[2], 1.0]);
                    set(&mut g, "opacity", KV::Scalar(*opacity));
                    set(&mut g, "rule", KV::Enum(u32::from(*even_odd)));
                    g
                }
                Item::GradientFill { radial, start, end, gradient, opacity, even_odd } => {
                    let mut g = build::shape_gradient_fill(ids, *radial, *start, *end, gradient.clone());
                    set(&mut g, "opacity", KV::Scalar(*opacity));
                    set(&mut g, "rule", KV::Enum(u32::from(*even_odd)));
                    g
                }
                Item::Stroke(s) => {
                    let mut g = match &s.gradient {
                        Some((radial, a, b, gr)) => build::shape_gradient_stroke(ids, *radial, *a, *b, gr.clone(), s.width),
                        None => build::shape_stroke(ids, [s.color[0], s.color[1], s.color[2], 1.0], s.width),
                    };
                    set(&mut g, "opacity", KV::Scalar(s.opacity));
                    set(&mut g, "cap", KV::Enum(s.cap));
                    set(&mut g, "join", KV::Enum(s.join));
                    set(&mut g, "miter", KV::Scalar(s.miter));
                    if let Some((d, gap)) = s.dash
                        && let Some(ds) = g.sub_mut("dashes")
                    {
                        set(ds, "dash", KV::Scalar(d));
                        set(ds, "gap", KV::Scalar(gap));
                    }
                    g
                }
                Item::Offset(a) => {
                    let mut g = build::shape_simple_op(ids, "offset")?;
                    set(&mut g, "amount", KV::Scalar(*a));
                    set(&mut g, "join", KV::Enum(0));
                    g
                }
            })
        })
        .collect()
}

fn shadow_style(l: &mut Layer, ids: &mut Ids, comp: &Comp, s: &Shadow) {
    let id = if s.inner { "innerShadow" } else { "dropShadow" };
    if effectcraft_project::styles::add_style(l, ids, id, &comp.global_light, true).is_none() {
        return;
    }
    let Some(g) = l.props.sub_mut(effectcraft_project::styles::GROUP).and_then(|ls| ls.sub_mut(id)) else { return };
    // Figma shadows blend normally.
    set(g, "blendMode", KV::Enum(0));
    set(g, "color", KV::Color([s.color[0], s.color[1], s.color[2], 1.0]));
    set(g, "opacity", KV::Scalar(s.opacity.clamp(0.0, 100.0)));
    set(g, "useGlobalLight", KV::Bool(false));
    set(g, "angle", KV::Scalar(s.angle));
    set(g, "distance", KV::Scalar(s.distance));
    set(g, "size", KV::Scalar(s.size.min(250.0)));
    set(g, if s.inner { "choke" } else { "spread" }, KV::Scalar(s.spread));
}

fn prop(l: &mut Layer, path: &str, v: KV) {
    if let Some(p) = l.props.prop_mut(path) {
        p.value = v;
    }
}

/// A rendered image's file format. PDF is the default (EffectCraft draws it as vector footage,
/// sharp at any scale); PNG is the fallback.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Fmt {
    Pdf,
    Png,
}

impl Fmt {
    fn ext(self) -> &'static str {
        match self {
            Fmt::Pdf => "pdf",
            Fmt::Png => "png",
        }
    }
    fn sniff(b: &[u8]) -> Option<Fmt> {
        if b.starts_with(b"%PDF-") {
            Some(Fmt::Pdf)
        } else if b.starts_with(b"\x89PNG\r\n\x1a\n") {
            Some(Fmt::Png)
        } else {
            None
        }
    }
}

/// The candidates of a payload image entry, PDF first: `{pdf: base64}`, `{pdfPath}`, `{png:
/// base64}`, `{path}` (PDF or PNG, by content), a data URL or a plain path, and `fallback` (another
/// entry tried after these).
fn entry_candidates(s: &Session, v: &Value, depth: usize, out: &mut Vec<std::result::Result<Vec<u8>, String>>) {
    let read = |p: &str| s.services.read_file(p).map_err(|e| format!("cannot read {p}: {e}"));
    match v {
        Value::String(x) if x.starts_with("data:") => out.push(fetch::base64_decode(x)),
        Value::String(x) => out.push(read(x)),
        Value::Object(o) => {
            let before = out.len();
            if let Some(b) = o.get("pdf").and_then(Value::as_str) {
                out.push(fetch::base64_decode(b));
            }
            if let Some(p) = o.get("pdfPath").and_then(Value::as_str) {
                out.push(read(p));
            }
            if let Some(p) = o.get("path").and_then(Value::as_str) {
                out.push(read(p));
            }
            if let Some(b) = o.get("png").or_else(|| o.get("base64")).and_then(Value::as_str) {
                out.push(fetch::base64_decode(b));
            }
            if let Some(f) = o.get("fallback").filter(|_| depth < 4) {
                entry_candidates(s, f, depth + 1, out);
            }
            if out.len() == before {
                out.push(Err("image entry needs `pdf`, `pdfPath`, `path` or `png`".into()));
            }
        }
        _ => out.push(Err("bad image entry".into())),
    }
}

/// Every planned image's candidates (bytes or why not), PDF ones first.
type Candidates = Vec<std::result::Result<Vec<u8>, String>>;

fn raster_candidates(s: &Session, plan: &Plan, src: &Source, scale: f64, notes: &mut Vec<String>) -> Vec<Candidates> {
    let mut out: Vec<Candidates> = vec![vec![]; plan.rasters.len()];
    let mut want = vec![];
    for (i, r) in plan.rasters.iter().enumerate() {
        match src.images.get(&r.node_id) {
            Some(v) => {
                if let Some(slot) = out.get_mut(i) {
                    entry_candidates(s, v, 0, slot);
                    // PDFs first, whatever the entry's order.
                    slot.sort_by_key(|c| !matches!(c, Ok(b) if Fmt::sniff(b) == Some(Fmt::Pdf)));
                }
            }
            None if src.rest.is_some() => want.push(i),
            None => notes.push(format!("{}: no image in the payload ({}), skipped", r.name, r.reason)),
        }
    }
    if let (Some((base, key, token)), false) = (&src.rest, want.is_empty()) {
        let ids: Vec<String> = want.iter().filter_map(|i| plan.rasters.get(*i).map(|r| r.node_id.clone())).collect();
        // PDF renders, then PNG for every image (the fallback when a PDF fails to import).
        for fmt in [Fmt::Pdf, Fmt::Png] {
            match fetch::images(base, key, &ids, scale, token, fmt.ext()) {
                Ok(got) => {
                    for (id, r) in got {
                        let Some(i) = want.iter().copied().find(|i| plan.rasters.get(*i).is_some_and(|x| x.node_id == id)) else { continue };
                        if let Some(slot) = out.get_mut(i) {
                            slot.push(r);
                        }
                    }
                }
                Err(e) => notes.push(format!("{} renders not fetched: {e}", fmt.ext().to_uppercase())),
            }
        }
    }
    out
}

/// Write and import the images, PDF first and PNG when that fails; each import is its own step
/// until [`fold`]. Per image: the footage item and the format used.
fn import_rasters(s: &mut Session, plan: &Plan, src: &Source, scale: f64, notes: &mut Vec<String>) -> Vec<Option<(ItemId, Fmt)>> {
    let cands = raster_candidates(s, plan, src, scale, notes);
    let dir = s.figma_assets_dir(&src.label);
    let mut items = vec![None; plan.rasters.len()];
    if cands.iter().all(Vec::is_empty) {
        return items;
    }
    if let Err(e) = std::fs::create_dir_all(&dir) {
        notes.push(format!("images not imported: cannot create {} ({e})", dir.display()));
        return items;
    }
    for (i, list) in cands.into_iter().enumerate() {
        let Some(r) = plan.rasters.get(i) else { continue };
        if list.is_empty() {
            continue;
        }
        let safe: String = r.node_id.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).take(60).collect();
        let nm: String = r.name.chars().filter(|c| c.is_alphanumeric() || matches!(c, ' ' | '-' | '_')).take(40).collect();
        let mut why = vec![];
        for c in list {
            let b = match c {
                Ok(b) => b,
                Err(e) => {
                    why.push(e);
                    continue;
                }
            };
            let Some(fmt) = Fmt::sniff(&b) else {
                why.push("not a PDF or PNG".into());
                continue;
            };
            let path = dir.join(format!("{} {safe}.{}", nm.trim(), fmt.ext())).to_string_lossy().into_owned();
            if let Err(e) = s.services.write_file(&path, &b) {
                why.push(format!("cannot write {path} ({e})"));
                continue;
            }
            match crate::commands::file::import(s, &json!({"paths": [path]})) {
                Ok(v) => match v["items"].get(0).and_then(Value::as_u64) {
                    Some(id) => {
                        items[i] = Some((ItemId(id), fmt));
                        break;
                    }
                    None => {
                        why.push(format!("{} import failed: {}", fmt.ext().to_uppercase(), v["errors"].get(0).and_then(Value::as_str).unwrap_or("no item")))
                    }
                },
                Err(e) => why.push(format!("{} import failed: {e}", fmt.ext().to_uppercase())),
            }
        }
        match items[i] {
            None => notes.push(format!("{}: image not imported ({})", r.name, why.join("; "))),
            Some((_, Fmt::Png)) if !why.is_empty() => notes.push(format!("{}: PNG used ({})", r.name, why.join("; "))),
            Some(_) => {}
        }
    }
    items
}

/// One layer for a planned layer (transform, contents, styles, effects, masks).
fn make_layer(
    proj: &mut Project,
    comp: &Comp,
    pl: &PLayer,
    comp_items: &[Option<ItemId>],
    comp_dims: &[(u32, u32)],
    rasters: &[Option<(ItemId, Fmt)>],
    dur: Tick,
) -> Option<Layer> {
    let (source, size, ldur) = match &pl.kind {
        PKind::Shape(_) => (LayerSource::Shape, (comp.width, comp.height), None),
        PKind::Text(_) => (LayerSource::Text, (comp.width, comp.height), None),
        PKind::Null => (LayerSource::Null, (100, 100), None),
        PKind::Precomp(i) => (LayerSource::Comp { item: (*comp_items.get(*i)?)? }, *comp_dims.get(*i)?, Some(dur)),
        PKind::Raster(i) => {
            let (item, _) = (*rasters.get(*i)?)?;
            let (w, h) = match &proj.item(item)?.kind {
                ItemKind::Footage(f) => (f.width, f.height),
                _ => return None,
            };
            (LayerSource::Footage { item }, (w, h), None)
        }
    };
    let mut l = build::layer(proj, comp, &pl.name, source, size, ldur);
    let mut anchor = pl.anchor;
    let mut scale = pl.scale;
    if let PKind::Raster(_) = &pl.kind {
        // The image covers the node's box (a PNG at `scale`×, a PDF at 1×): fit it back to the box.
        let (iw, ih) = (size.0.max(1) as f64, size.1.max(1) as f64);
        anchor = [iw / 2.0, ih / 2.0];
        scale = [scale[0] * (pl.anchor[0] * 2.0) / iw, scale[1] * (pl.anchor[1] * 2.0) / ih];
    }
    prop(&mut l, "transform/anchor", KV::Vec3([anchor[0], anchor[1], 0.0]));
    prop(&mut l, "transform/position", KV::Vec3([pl.position[0], pl.position[1], 0.0]));
    prop(&mut l, "transform/rotation", KV::Scalar(pl.rotation));
    prop(&mut l, "transform/scale", KV::Vec3([scale[0], scale[1], 100.0]));
    prop(&mut l, "transform/opacity", KV::Scalar(pl.opacity));
    l.blend_mode = pl.blend;
    l.switches.video = pl.visible && !pl.is_matte;
    // PDF renders are vector footage: Continuously Rasterize keeps them sharp when scaled.
    if let PKind::Raster(i) = &pl.kind
        && let Some(Some((_, Fmt::Pdf))) = rasters.get(*i)
    {
        l.switches.collapse = true;
    }
    let mut ids = Ids(&mut proj.next_id);
    match &pl.kind {
        PKind::Shape(contents) => {
            let groups = items(&mut ids, contents);
            if let Some(c) = l.props.sub_mut("contents") {
                c.children.extend(groups.into_iter().map(Into::into));
            }
        }
        PKind::Text(doc) => prop(&mut l, "text/sourceText", KV::Text(doc.clone())),
        _ => {}
    }
    for s in &pl.shadows {
        shadow_style(&mut l, &mut ids, comp, s);
    }
    if let Some(b) = pl.blur.filter(|b| *b > 0.0)
        && let Some(spec) = effectcraft_effects::lookup("ec.blur.gaussian")
    {
        let mut g = effectcraft_effects::instantiate(spec, &mut ids, spec.name, [size.0 as f64, size.1 as f64]);
        set(&mut g, "blurriness", KV::Scalar(b));
        if let Some(fx) = l.props.sub_mut("effects") {
            fx.children.push(g.into());
        }
    }
    for (k, sp) in pl.masks.iter().enumerate() {
        let mk = build::mask(&mut ids, &format!("Mask {}", k + 1), sp.clone(), MaskMode::Add, [255, 255, 0]);
        if let Some(m) = l.props.sub_mut("masks") {
            m.children.push(mk.into());
        }
    }
    Some(l)
}

/// Fold the undo steps recorded since `snap` into one.
fn fold(s: &mut Session, snap: &Arc<Project>, label: &str) {
    let h = &mut s.history;
    let Some(start) = h.undo.iter().rposition(|(_, p)| Arc::ptr_eq(p, snap)) else { return };
    let first = h.undo[start].1.clone();
    h.undo.truncate(start);
    h.undo.push((label.to_string(), first));
    h.merge_key = None;
}

pub(super) fn run(s: &mut Session, plan: Plan, src: &Source, scale: f64, cfg: &Settings) -> Result<Value> {
    let (project, history, state) = (s.project.clone(), s.history.clone(), s.state.clone());
    let mut notes = plan.notes.clone();
    let rasters = import_rasters(s, &plan, src, scale, &mut notes);
    let rate = FrameRate::from_f64(cfg.frame_rate);
    let dur = rate.snap_nearest(Tick::from_seconds_f64(cfg.duration));
    let into = cfg.into;
    let label = if plan.name.trim().is_empty() { cfg.origin.to_string() } else { plan.name.clone() };
    let raster_items = rasters.clone();
    let built = s.edit(cfg.undo_label, None, |proj, st| {
        // Precomps and images go in a folder; the new top-level comps stay at the root.
        let nested = plan.comps.len().saturating_sub(plan.roots.len() + usize::from(plan.into_active.is_some()));
        let folder = (nested > 0 || raster_items.iter().any(Option::is_some))
            .then(|| proj.add_item(&format!("{label} ({})", cfg.origin), Label::Yellow, None, ItemKind::Folder));
        let mut comp_items: Vec<Option<ItemId>> = vec![None; plan.comps.len()];
        let comp_dims: Vec<(u32, u32)> = plan.comps.iter().map(|c| (c.width, c.height)).collect();
        for (i, pc) in plan.comps.iter().enumerate() {
            if Some(i) == plan.into_active {
                comp_items[i] = into;
                continue;
            }
            let parent = if plan.roots.contains(&i) { None } else { folder };
            let c = Comp::new(pc.width, pc.height, rate, dur);
            comp_items[i] = Some(proj.add_item(&pc.name, Label::Sandstone, parent, ItemKind::Comp(c.into())));
        }
        if let Some(f) = folder {
            for (it, _) in raster_items.iter().flatten() {
                if let Some(item) = proj.items.get_mut(it) {
                    item.parent = Some(f);
                }
            }
        }
        let mut top_layers = vec![];
        for (i, pc) in plan.comps.iter().enumerate() {
            let Some(cid) = comp_items.get(i).copied().flatten() else { continue };
            let comp = proj.comp(cid).ok_or(EngineError::NoComp)?.clone();
            let made: Vec<Option<Layer>> = pc
                .layers
                .iter()
                .map(|pl| {
                    make_layer(proj, &comp, pl, &comp_items, &comp_dims, &raster_items, dur).map(|mut layer| {
                        layer.comment = format!("{} {}", cfg.origin, pl.figma_id);
                        layer
                    })
                })
                .collect();
            let lids: Vec<Option<LayerId>> = made.iter().map(|l| l.as_ref().map(|l| l.id)).collect();
            let mut layers: Vec<Layer> = vec![];
            for (k, l) in made.into_iter().enumerate() {
                let (Some(mut l), Some(pl)) = (l, pc.layers.get(k)) else { continue };
                if let Some(p) = pl.parent
                    && let (Some(Some(pid)), Some(pp)) = (lids.get(p), pc.layers.get(p))
                {
                    l.parent = Some(*pid);
                    // Comp space → the parent null's layer space (nulls are never rotated or
                    // scaled here, but nested nulls are parented in turn: their own comp-space
                    // positions are what we subtract).
                    let pos = [pl.position[0] - pp.position[0] + pp.anchor[0], pl.position[1] - pp.position[1] + pp.anchor[1]];
                    prop(&mut l, "transform/position", KV::Vec3([pos[0], pos[1], 0.0]));
                }
                if let Some((m, kind)) = pl.matte
                    && let Some(Some(mid)) = lids.get(m)
                {
                    l.track_matte = Some(TrackMatte { layer: *mid, kind });
                }
                layers.push(l);
            }
            // Figma paints bottom first; the Timeline lists the top layer first.
            layers.reverse();
            let ids: Vec<LayerId> = layers.iter().map(|l| l.id).collect();
            let target = proj.comp_mut(cid).ok_or(EngineError::NoComp)?;
            if Some(i) == plan.into_active {
                for (k, l) in layers.into_iter().enumerate() {
                    target.layers.insert(k, l);
                }
            } else {
                target.layers = layers.into();
            }
            if Some(i) == plan.into_active || plan.roots.first() == Some(&i) {
                top_layers = ids;
            }
        }
        if plan.into_active.is_some() {
            st.selected_layers = top_layers.clone();
        }
        Ok((comp_items, top_layers))
    });
    let (comp_items, top_layers) = match built {
        Ok(v) => v,
        Err(e) => {
            s.project = project;
            s.history = history;
            s.state = state;
            s.sanitize_state();
            s.bump();
            return Err(e);
        }
    };
    fold(s, &project, cfg.undo_label);
    let roots: Vec<u64> = plan.roots.iter().filter_map(|i| comp_items.get(*i).copied().flatten()).map(|c| c.0).collect();
    let precomps: Vec<u64> =
        comp_items.iter().enumerate().filter(|(i, _)| !plan.roots.contains(i) && Some(*i) != plan.into_active).filter_map(|(_, c)| c.map(|c| c.0)).collect();
    let comp = into.map(|c| c.0).or(roots.first().copied());
    if into.is_none()
        && let Some(c) = roots.first()
    {
        s.open_comp(ItemId(*c));
    }
    let (native, _) = plan.counts();
    let images = rasters.iter().filter(|r| r.is_some()).count();
    if images > 0 || native > 0 {
        s.toast(format!("Imported from {}: {native} native layer(s), {images} image(s)", cfg.origin));
    }
    Ok(json!({
        "comp": comp,
        "comps": roots,
        "precomps": precomps,
        "layers": top_layers.iter().map(|l| l.0).collect::<Vec<_>>(),
        "native": native,
        "images": images,
        "rasterized": plan.rasters.iter().zip(&rasters).map(|(r, it)| json!({"node": r.node_id, "name": r.name, "reason": r.reason, "item": it.map(|(i, _)| i.0), "format": it.map(|(_, f)| f.ext())})).collect::<Vec<_>>(),
        "notes": notes,
    }))
}
