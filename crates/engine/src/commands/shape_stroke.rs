//! Shape Stroke options: the Dashes "+" / "−" buttons (Dash 2 / Gap 2, Dash 3 / Gap 3), Taper and
//! Wave. Taper and Wave are ordinary properties of the stroke (`…/stroke/taper/startLength`); the
//! commands here set several of them in one undo step and add the groups to strokes saved before
//! they existed. Also the toolbar's Fill / Stroke options on selected shape layers.

use effectcraft_color::BlendMode;
use effectcraft_keyframe::{Gradient, Value as KV};
use effectcraft_project::build::{self, EXTRA_DASHES, Ids};
use effectcraft_project::{ItemId, Layer, LayerId, LayerSource, Node, PropGroup, Uid};
use effectcraft_time::Tick;
use serde_json::{Value, json};

use super::layer::color_p;
use super::shape_tool::PaintKind;
use super::{CommandSpec, bad, f_p, has_layers, layer_mut, layer_p, layers_p, merge_p};
use crate::{EngineError, Result, Session, cmd};

fn is_stroke(g: &PropGroup) -> bool {
    matches!(g.match_id.as_str(), "stroke" | "gstroke")
}

/// `{layer?, prop?: stroke uid}` → the stroke; without `prop`, the first stroke of the layer (a
/// selected stroke property wins).
fn stroke_ref(s: &Session, p: &Value, cmd: &str) -> Result<(ItemId, LayerId, Uid)> {
    let (cid, lid) = layer_p(s, p, cmd)?;
    let l = s.project.comp(cid).and_then(|c| c.layer(lid)).ok_or(EngineError::NoComp)?;
    if let Some(uid) = p.get("prop").and_then(Value::as_u64) {
        let g = l.props.find_group(uid).ok_or_else(|| bad(cmd, format!("no property group @{uid}")))?;
        if !is_stroke(g) {
            return Err(bad(cmd, format!("`{}` is not a Stroke or Gradient Stroke", g.name)));
        }
        return Ok((cid, lid, uid));
    }
    let contents = l.props.sub("contents").ok_or_else(|| bad(cmd, "not a shape layer"))?;
    // A selected stroke (or a property inside one), else the first stroke in the contents.
    let mut found = None;
    for (sl, su) in &s.state.selected_props {
        if *sl == lid
            && let Some(g) = find_stroke_containing(contents, *su)
        {
            found = Some(g);
            break;
        }
    }
    if found.is_none() {
        let mut first = None;
        walk_groups(contents, &mut |g| {
            if first.is_none() && is_stroke(g) {
                first = Some(g.uid);
            }
        });
        found = first;
    }
    found.map(|u| (cid, lid, u)).ok_or_else(|| bad(cmd, "the layer has no stroke"))
}

fn walk_groups(g: &PropGroup, f: &mut impl FnMut(&PropGroup)) {
    for sub in g.groups() {
        f(sub);
        walk_groups(sub, f);
    }
}

fn find_stroke_containing(contents: &PropGroup, uid: Uid) -> Option<Uid> {
    let mut out = None;
    walk_groups(contents, &mut |g| {
        if out.is_none() && is_stroke(g) && (g.uid == uid || g.find(uid).is_some() || g.find_group(uid).is_some()) {
            out = Some(g.uid);
        }
    });
    out
}

fn dashes_add(s: &mut Session, p: &Value) -> Result<Value> {
    let c = "shape.dashes.add";
    let (cid, lid, uid) = stroke_ref(s, p, c)?;
    s.edit("Add Dash", None, |proj, _| {
        let mut next = proj.next_id;
        let g = layer_mut(proj, cid, lid)?.props.find_group_mut(uid).ok_or_else(|| bad(c, "stroke vanished"))?;
        let d = g.sub_mut("dashes").ok_or_else(|| bad(c, "the stroke has no Dashes group"))?;
        // Before the first "+", Dash is 0 (no dashes): the first "+" turns it on.
        let dash = d.get("dash").map(|p| p.value.as_f64()).unwrap_or(0.0);
        let mut ids = Ids(&mut next);
        let added = if dash <= 0.0 {
            if let Some(p) = d.get_mut("dash") {
                p.value = KV::Scalar(10.0);
            }
            "dash"
        } else {
            let Some((dm, gm)) = EXTRA_DASHES.iter().find(|(dm, _)| d.get(dm).is_none()) else {
                return Err(bad(c, "a stroke has at most three dash/gap pairs"));
            };
            let k = if *dm == "dash2" { 2 } else { 3 };
            // Insert before Offset, which stays last.
            let at = d.children.iter().position(|n| n.match_id() == "offset").unwrap_or(d.children.len());
            let dv = d.get("dash").map(|p| p.value.as_f64()).unwrap_or(10.0);
            let gv = d.get("gap").map(|p| p.value.as_f64()).filter(|g| *g > 0.0).unwrap_or(dv);
            d.children.insert(at, ids.prop(gm, &format!("Gap {k}"), KV::Scalar(gv)).with_ui(effectcraft_project::ParamUi::Pixels).into());
            d.children.insert(at, ids.prop(dm, &format!("Dash {k}"), KV::Scalar(dv)).with_ui(effectcraft_project::ParamUi::Pixels).into());
            dm
        };
        proj.next_id = next;
        Ok(json!({"added": added}))
    })
}

fn dashes_remove(s: &mut Session, p: &Value) -> Result<Value> {
    let c = "shape.dashes.remove";
    let (cid, lid, uid) = stroke_ref(s, p, c)?;
    s.edit("Remove Dash", None, |proj, _| {
        let g = layer_mut(proj, cid, lid)?.props.find_group_mut(uid).ok_or_else(|| bad(c, "stroke vanished"))?;
        let d = g.sub_mut("dashes").ok_or_else(|| bad(c, "the stroke has no Dashes group"))?;
        for (dm, gm) in EXTRA_DASHES.iter().rev() {
            if d.get(dm).is_some() {
                d.children.retain(|n| n.match_id() != *dm && n.match_id() != *gm);
                return Ok(json!({"removed": dm}));
            }
        }
        // The last "−" turns dashes off.
        if let Some(p) = d.get_mut("dash") {
            p.value = KV::Scalar(0.0);
            p.keys.clear();
        }
        Ok(json!({"removed": "dash"}))
    })
}

/// Set `fields` (`param name → (match id, kind)`) of the stroke's `group`, creating the group.
fn set_group(s: &mut Session, p: &Value, c: &str, group: &str, fields: &[(&str, &str, bool)]) -> Result<Value> {
    let (cid, lid, uid) = stroke_ref(s, p, c)?;
    let t = s.time();
    let label = if group == "taper" { "Stroke Taper" } else { "Stroke Wave" };
    s.edit(label, merge_p(p), |proj, _| {
        let mut next = proj.next_id;
        {
            let g = layer_mut(proj, cid, lid)?.props.find_group_mut(uid).ok_or_else(|| bad(c, "stroke vanished"))?;
            if g.sub(group).is_none() {
                let mut ids = Ids(&mut next);
                let ng = if group == "taper" { build::stroke_taper(&mut ids) } else { build::stroke_wave(&mut ids) };
                g.children.push(ng.into());
            }
        }
        proj.next_id = next;
        let l = layer_mut(proj, cid, lid)?;
        let lt = l.layer_time(t);
        let g = l.props.find_group_mut(uid).ok_or_else(|| bad(c, "stroke vanished"))?;
        let sub = g.sub_mut(group).ok_or_else(|| bad(c, "group vanished"))?;
        let mut out = serde_json::Map::new();
        for (param, m, is_enum) in fields {
            let Some(v) = p.get(*param) else { continue };
            let pr = sub.get_mut(m).ok_or_else(|| bad(c, format!("no {m}")))?;
            let nv = if *is_enum {
                let opts: &[&str] = if *m == "units" && group == "taper" { &["pixels", "percent"] } else { &["pixels", "cycles"] };
                let i = match v {
                    Value::String(s) => {
                        opts.iter().position(|o| o.eq_ignore_ascii_case(s)).ok_or_else(|| bad(c, format!("`{param}` is one of {opts:?}")))? as u32
                    }
                    Value::Number(n) => n.as_u64().unwrap_or(0).min(opts.len() as u64 - 1) as u32,
                    _ => return Err(bad(c, format!("bad `{param}`"))),
                };
                KV::Enum(i)
            } else {
                let x = v.as_f64().ok_or_else(|| bad(c, format!("`{param}` must be a number")))?;
                let x = if let effectcraft_project::ParamUi::Slider { min, max, .. } = pr.ui { x.clamp(min, max) } else { x.max(0.0) };
                KV::Scalar(x)
            };
            pr.set_value_at(lt, nv.clone());
            out.insert(param.to_string(), nv.to_json());
        }
        Ok(json!({"stroke": uid, group: Value::Object(out)}))
    })
}

fn taper(s: &mut Session, p: &Value) -> Result<Value> {
    set_group(
        s,
        p,
        "shape.stroke.taper",
        "taper",
        &[
            ("units", "units", true),
            ("startLength", "startLength", false),
            ("endLength", "endLength", false),
            ("startWidth", "startWidth", false),
            ("endWidth", "endWidth", false),
            ("startEase", "startEase", false),
            ("endEase", "endEase", false),
        ],
    )
}

fn wave(s: &mut Session, p: &Value) -> Result<Value> {
    set_group(
        s,
        p,
        "shape.stroke.wave",
        "wave",
        &[("amount", "amount", false), ("units", "units", true), ("wavelength", "wavelength", false), ("cycles", "cycles", false), ("phase", "phase", false)],
    )
}

/// Path items a Fill or Stroke paints in their group.
const PATHS: [&str; 4] = ["rect", "ellipse", "star", "path"];

/// Shape groups nest; deeper contents are left alone (never-crash recursion bound).
pub(crate) const MAX_DEPTH: usize = 64;

/// The first Fill colour, Stroke colour and Stroke Width in `layer`'s contents at layer time
/// `lt` (what the toolbar shows for a selected shape layer).
pub fn first_paint(layer: &Layer, lt: Tick) -> (Option<[f64; 4]>, Option<[f64; 4]>, Option<f64>) {
    let (mut fill, mut stroke, mut width) = (None, None, None);
    if let Some(c) = layer.props.sub("contents") {
        walk_groups(c, &mut |g| {
            let at = |id: &str| g.get(id).map(|p| p.value_at(lt));
            let rgba = |v: KV| v.as_color().map(f64::from);
            match g.match_id.as_str() {
                "fill" if fill.is_none() => fill = at("color").map(rgba),
                "stroke" if stroke.is_none() => {
                    stroke = at("color").map(rgba);
                    width = at("width").map(|v| v.as_f64());
                }
                _ => {}
            }
        });
    }
    (fill, stroke, width)
}

/// The Fill or Stroke kind of the first fill / stroke item in `layer`'s contents (what the
/// toolbar's swatch shows for a selected shape layer): None when it is switched off or there is
/// none, Solid Color for a Fill / Stroke, Linear or Radial for a Gradient Fill / Stroke.
pub fn first_kinds(layer: &Layer) -> (PaintKind, PaintKind) {
    let (mut fill, mut stroke) = (None, None);
    let kind = |g: &PropGroup| match g.match_id.as_str() {
        _ if !g.enabled => PaintKind::None,
        "fill" | "stroke" => PaintKind::Solid,
        _ if g.get("type").is_some_and(|t| t.value == KV::Enum(1)) => PaintKind::Radial,
        _ => PaintKind::Linear,
    };
    if let Some(c) = layer.props.sub("contents") {
        walk_groups(c, &mut |g| match g.match_id.as_str() {
            "fill" | "gfill" if fill.is_none() => fill = Some(kind(g)),
            "stroke" | "gstroke" if stroke.is_none() => stroke = Some(kind(g)),
            _ => {}
        });
    }
    (fill.unwrap_or(PaintKind::None), stroke.unwrap_or(PaintKind::None))
}

/// What `shape.fillStroke` changes on the fills or on the strokes.
#[derive(Clone, Copy, Debug, Default)]
struct Side {
    /// A Fill's / Stroke's colour.
    color: Option<[f64; 4]>,
    /// Switch off (None), or make every item a Fill / Stroke or a Gradient Fill / Stroke.
    kind: Option<PaintKind>,
    /// Index into `BlendMode::ALL`.
    blend: Option<u32>,
    /// Opacity (%).
    opacity: Option<f64>,
    /// The colour of a Fill / Stroke made from a gradient one, or added.
    solid: [f64; 4],
    /// Paths with no Fill / Stroke (in their group or one around it) get one.
    add: bool,
}

/// One `shape.fillStroke` edit.
#[derive(Clone, Copy, Debug)]
struct Edit {
    fill: Side,
    stroke: Side,
    width: Option<f64>,
    /// The width of an added stroke.
    new_width: f64,
}

/// Properties a Fill and a Gradient Fill (or a Stroke and a Gradient Stroke) don't share in
/// meaning, so a converted item takes the new kind's defaults for them.
const PAINT_ONLY: [&str; 7] = ["color", "colors", "type", "start", "end", "highlightLength", "highlightAngle"];

fn is_fill(g: &PropGroup) -> bool {
    matches!(g.match_id.as_str(), "fill" | "gfill")
}

/// `[x, y, w, h]` around the group's own path items at layer time `lt` (a Gradient's span).
fn path_bounds(c: &PropGroup, lt: Tick) -> [f64; 4] {
    let (mut lo, mut hi) = ([f64::INFINITY; 2], [f64::NEG_INFINITY; 2]);
    let mut add = |p: [f64; 2]| {
        if p.iter().all(|v| v.is_finite()) {
            for i in 0..2 {
                lo[i] = lo[i].min(p[i]);
                hi[i] = hi[i].max(p[i]);
            }
        }
    };
    for g in c.groups() {
        let at = |id: &str| g.get(id).map(|p| p.value_at(lt));
        let v2 = |id: &str, d: [f64; 2]| at(id).map(|v| v.as_vec2()).unwrap_or(d);
        match g.match_id.as_str() {
            "rect" | "ellipse" => {
                let (pos, size) = (v2("position", [0.0; 2]), v2("size", [100.0; 2]));
                add([pos[0] - size[0] / 2.0, pos[1] - size[1] / 2.0]);
                add([pos[0] + size[0] / 2.0, pos[1] + size[1] / 2.0]);
            }
            "star" => {
                let pos = v2("position", [0.0; 2]);
                let r = at("outerRadius").map(|v| v.as_f64()).unwrap_or(50.0).abs();
                add([pos[0] - r, pos[1] - r]);
                add([pos[0] + r, pos[1] + r]);
            }
            "path" => {
                if let Some(KV::Path(sp)) = at("path") {
                    sp.vertices.iter().for_each(|v| add(*v));
                }
            }
            _ => {}
        }
    }
    if lo[0] <= hi[0] && lo[1] <= hi[1] { [lo[0], lo[1], hi[0] - lo[0], hi[1] - lo[1]] } else { [-50.0, -50.0, 100.0, 100.0] }
}

/// A gradient's Start and End Points across `bounds`: left to right, or out from the centre (as
/// the shape tools draw them).
fn gradient_ends(bounds: [f64; 4], radial: bool) -> ([f64; 2], [f64; 2]) {
    let [x, y, w, h] = bounds;
    let mid = [x + w / 2.0, y + h / 2.0];
    if radial { (mid, [mid[0] + w.max(h).max(2.0) / 2.0, mid[1]]) } else { ([x, mid[1]], [x + w.max(1.0), mid[1]]) }
}

/// A new Fill / Stroke / Gradient Fill / Gradient Stroke of `kind` (not None).
fn new_item(ids: &mut Ids, fill: bool, kind: PaintKind, color: [f64; 4], width: f64, bounds: [f64; 4]) -> PropGroup {
    let radial = kind == PaintKind::Radial;
    let (a, b) = gradient_ends(bounds, radial);
    match (fill, kind) {
        (true, PaintKind::Solid) => build::shape_fill(ids, color),
        (false, PaintKind::Solid) => build::shape_stroke(ids, color, width),
        (true, _) => build::shape_gradient_fill(ids, radial, a, b, Gradient::default()),
        (false, _) => build::shape_gradient_stroke(ids, radial, a, b, Gradient::default(), width),
    }
}

/// After Effects' base names of the four paint items.
fn base_name(match_id: &str) -> &'static str {
    match match_id {
        "fill" => "Fill",
        "gfill" => "Gradient Fill",
        "stroke" => "Stroke",
        _ => "Gradient Stroke",
    }
}

/// Set a Fill / Stroke item's kind: None switches it off (its eye, as After Effects does; it is
/// kept), the others switch it on, and turn a Fill into a Gradient Fill and back (Stroke alike).
/// A converted item keeps its uid-carrying shared properties (blend mode, opacity, width,
/// dashes, taper, …) and its number ("Fill 2" → "Gradient Fill 2"), or its custom name.
fn set_kind(g: &mut PropGroup, ids: &mut Ids, kind: PaintKind, solid: [f64; 4], bounds: [f64; 4]) {
    if kind == PaintKind::None {
        g.enabled = false;
        return;
    }
    g.enabled = true;
    let fill = is_fill(g);
    let gradient = matches!(g.match_id.as_str(), "gfill" | "gstroke");
    let radial = kind == PaintKind::Radial;
    match (gradient, kind) {
        (false, PaintKind::Solid) => {}
        (true, PaintKind::Linear | PaintKind::Radial) => {
            if let Some(t) = g.get_mut("type") {
                t.value = KV::Enum(u32::from(radial));
                t.keys.clear();
            }
        }
        _ => {
            let width = g.get("width").map(|w| w.value.as_f64()).unwrap_or(2.0);
            let mut n = new_item(ids, fill, kind, solid, width, bounds);
            for child in &mut n.children {
                let id = child.match_id().to_string();
                if PAINT_ONLY.contains(&id.as_str()) {
                    continue;
                }
                if let Some(old) = g.children.iter().find(|o| o.match_id() == id) {
                    *child = old.clone();
                }
            }
            let (old_base, new_base) = (base_name(&g.match_id), base_name(&n.match_id));
            n.name = match g.name.strip_prefix(old_base) {
                Some(rest) if rest.is_empty() || rest.starts_with(' ') => format!("{new_base}{rest}"),
                _ => g.name.clone(),
            };
            *g = n;
        }
    }
}

/// Apply a Side's colour, blend mode and opacity to one item (after its kind).
fn set_side(g: &mut PropGroup, side: &Side, lt: Tick) {
    let solid = matches!(g.match_id.as_str(), "fill" | "stroke");
    let mut set = |id: &str, v: Option<KV>| {
        if let (Some(v), Some(p)) = (v, g.get_mut(id)) {
            p.set_value_at(lt, v);
        }
    };
    if solid {
        set("color", side.color.map(KV::Color));
    }
    set("opacity", side.opacity.map(KV::Scalar));
    if let (Some(b), Some(p)) = (side.blend, g.get_mut("blend")) {
        p.value = KV::Enum(b);
        p.keys.clear();
    }
}

/// Paint one contents group and its sub-groups with `e`. `painted` says whether a group around
/// this one already has a (fill, stroke), so its paths need none of their own.
fn paint(c: &mut PropGroup, ids: &mut Ids, lt: Tick, e: &Edit, painted: (bool, bool), depth: usize) {
    if depth > MAX_DEPTH {
        return;
    }
    let bounds = path_bounds(c, lt);
    let has_path = c.groups().any(|g| PATHS.contains(&g.match_id.as_str()));
    let painted = (painted.0 || c.groups().any(is_fill), painted.1 || c.groups().any(is_stroke));
    let adds = |s: &Side| s.add && s.kind != Some(PaintKind::None);
    // A path with no stroke gets one before its fills, and one with no fill gets one after its
    // paths and strokes (as new shapes have them: paths, stroke, fill).
    if has_path && !painted.1 && adds(&e.stroke) {
        let at = c.children.iter().position(|n| matches!(n, Node::Group(g) if is_fill(g))).unwrap_or(c.children.len());
        let kind = e.stroke.kind.unwrap_or(PaintKind::Solid);
        let mut g = new_item(ids, false, kind, e.stroke.color.unwrap_or(e.stroke.solid), e.width.unwrap_or(e.new_width), bounds);
        set_side(&mut g, &e.stroke, lt);
        c.children.insert(at, g.into());
    }
    if has_path && !painted.0 && adds(&e.fill) {
        let after = |n: &Node| matches!(n, Node::Group(g) if PATHS.contains(&g.match_id.as_str()) || is_stroke(g));
        let at = c.children.iter().rposition(after).map_or(c.children.len(), |i| i + 1);
        let kind = e.fill.kind.unwrap_or(PaintKind::Solid);
        let mut g = new_item(ids, true, kind, e.fill.color.unwrap_or(e.fill.solid), 0.0, bounds);
        set_side(&mut g, &e.fill, lt);
        c.children.insert(at, g.into());
    }
    for n in &mut c.children {
        let Node::Group(g) = n else { continue };
        let side = match g.match_id.as_str() {
            "fill" | "gfill" => &e.fill,
            "stroke" | "gstroke" => &e.stroke,
            "group" => {
                if let Some(sub) = g.sub_mut("contents") {
                    paint(sub, ids, lt, e, painted, depth + 1);
                }
                continue;
            }
            _ => continue,
        };
        if let Some(k) = side.kind {
            set_kind(g, ids, k, side.color.unwrap_or(side.solid), bounds);
        }
        set_side(g, side, lt);
        if is_stroke(g)
            && let (Some(w), Some(p)) = (e.width, g.get_mut("width"))
        {
            p.set_value_at(lt, KV::Scalar(w));
        }
    }
}

/// `fill` / `stroke`, `fillType` / …, `fillBlend` / …, `fillOpacity` / … of `p`.
fn side_p(p: &Value, key: &str, solid: [f64; 4], c: &str) -> Result<Side> {
    let mut side = Side { solid, ..Side::default() };
    match p.get(key) {
        None => {}
        Some(Value::Null | Value::Bool(false)) => side.kind = Some(PaintKind::None),
        Some(_) => {
            let rgb = color_p(p, key).ok_or_else(|| bad(c, format!("`{key}`: a colour [r, g, b] or #hex, or false for none")))?;
            side.color = Some([rgb[0] as f64, rgb[1] as f64, rgb[2] as f64, 1.0]);
        }
    }
    if let Some(v) = p.get(format!("{key}Type").as_str()) {
        side.kind = Some(v.as_str().and_then(PaintKind::from_name).ok_or_else(|| bad(c, format!("`{key}Type`: none|solid|linear|radial")))?);
    }
    if let Some(v) = p.get(format!("{key}Blend").as_str()) {
        let m = v.as_str().and_then(BlendMode::from_name).ok_or_else(|| bad(c, format!("`{key}Blend`: a blend mode name (normal, multiply…)")))?;
        side.blend = BlendMode::ALL.iter().position(|x| *x == m).map(|i| i as u32);
    }
    if let Some(v) = p.get(format!("{key}Opacity").as_str()) {
        side.opacity = Some(v.as_f64().filter(|v| v.is_finite()).ok_or_else(|| bad(c, format!("`{key}Opacity`: a percentage")))?.clamp(0.0, 100.0));
    }
    Ok(side)
}

/// The toolbar's Fill and Stroke options with shape layers selected (as in After Effects, they
/// change the layers' fills and strokes, in nested groups too): `fill` / `stroke` set every Fill's
/// / Stroke's colour, `strokeWidth` every stroke's width; `fillType` / `strokeType` switch every
/// fill / stroke off (`none`, or `fill: false`: its eye, it is kept) or on as a Fill / Stroke
/// (`solid`) or a Gradient Fill / Stroke (`linear` / `radial`), converting it; `fillBlend` /
/// `strokeBlend` and `fillOpacity` / `strokeOpacity` set their blend mode and opacity. Paths with
/// no fill get one for a fill kind, and with no stroke one for a stroke colour, kind or a width
/// above 0. One undo step. Without values, reports the first layer's paint.
fn fill_stroke(s: &mut Session, p: &Value) -> Result<Value> {
    let c = "shape.fillStroke";
    let (cid, lids) = layers_p(s, p)?;
    let comp = s.project.comp(cid).ok_or(EngineError::NoComp)?;
    let shapes: Vec<LayerId> = lids.into_iter().filter(|l| comp.layer(*l).is_some_and(|l| matches!(l.source, LayerSource::Shape))).collect();
    if shapes.is_empty() {
        return Err(bad(c, "select a shape layer"));
    }
    let tool = &s.state.shape_tool;
    let rgba = |c: [f32; 3]| [c[0] as f64, c[1] as f64, c[2] as f64, 1.0];
    let mut fill = side_p(p, "fill", rgba(tool.fill.color), c)?;
    let mut stroke = side_p(p, "stroke", rgba(tool.stroke.color), c)?;
    let width = f_p(p, "strokeWidth").filter(|w| w.is_finite()).map(|w| w.max(0.0));
    let new_width = width.unwrap_or(if tool.stroke_width > 0.0 { tool.stroke_width } else { 2.0 });
    fill.add = fill.kind.is_some_and(|k| k != PaintKind::None);
    stroke.add = width.map_or(stroke.color.is_some() || stroke.kind.is_some(), |w| w > 0.0);
    let changes = |s: &Side| s.color.is_some() || s.kind.is_some() || s.blend.is_some() || s.opacity.is_some();
    if changes(&fill) || changes(&stroke) || width.is_some() {
        let e = Edit { fill, stroke, width, new_width };
        let t = s.time();
        s.edit("Fill and Stroke", merge_p(p), |proj, _| {
            let mut next = proj.next_id;
            for lid in &shapes {
                let l = layer_mut(proj, cid, *lid)?;
                let lt = l.layer_time(t);
                if let Some(contents) = l.props.sub_mut("contents") {
                    paint(contents, &mut Ids(&mut next), lt, &e, (false, false), 0);
                }
            }
            proj.next_id = next;
            Ok(())
        })?;
    }
    let l = shapes.first().and_then(|id| s.project.comp(cid)?.layer(*id)).ok_or(EngineError::NoComp)?;
    let (fill, stroke, width) = first_paint(l, l.layer_time(s.time()));
    let (fill_type, stroke_type) = first_kinds(l);
    Ok(json!({"fill": fill, "stroke": stroke, "strokeWidth": width, "fillType": fill_type.id(), "strokeType": stroke_type.id()}))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(
            "shape.fillStroke",
            "Fill and Stroke",
            [],
            None,
            "{layers?, fill?: color|false (none), stroke?: color|false, fillType?/strokeType?: none|solid|linear|radial (none switches the items off; solid/linear/radial switch them on, converting Fill ↔ Gradient Fill, Stroke ↔ Gradient Stroke), fillBlend?/strokeBlend?: blend mode, fillOpacity?/strokeOpacity? %, strokeWidth?, merge?} → {fill, stroke, strokeWidth, fillType, strokeType} (first layer)",
            has_layers,
            fill_stroke
        ),
        cmd!("shape.dashes.add", "Add Dash or Gap", [], None, "{layer?, prop?: stroke uid}", has_layers, dashes_add),
        cmd!("shape.dashes.remove", "Remove Dash or Gap", [], None, "{layer?, prop?: stroke uid}", has_layers, dashes_remove),
        cmd!(
            "shape.stroke.taper",
            "Stroke Taper",
            [],
            None,
            "{layer?, prop?: stroke uid, units?: pixels|percent, startLength?, endLength?, startWidth? (%), endWidth? (%), startEase? (%), endEase? (%)}",
            has_layers,
            taper
        ),
        cmd!(
            "shape.stroke.wave",
            "Stroke Wave",
            [],
            None,
            "{layer?, prop?: stroke uid, amount? (%), units?: pixels|cycles, wavelength?, cycles?, phase? (°)}",
            has_layers,
            wave
        ),
    ]
}
