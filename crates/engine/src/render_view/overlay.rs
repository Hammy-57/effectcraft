//! Viewer overlays drawn over agent renders: layer bounds (with a `#n name` tag), anchor points,
//! motion paths (the anchor's comp-space path with keyframe squares), title/action-safe margins
//! and the grid, in the layers' label colours as the Composition viewer draws them. Pure raster
//! code on straight 8-bit RGBA (no UI toolkit), anti-aliased through the path rasteriser.
//!
//! [`layer_geometry`] measures layers in comp space (content bounds through the layer → comp
//! matrix, like Layer ▸ sourceRectAtTime mapped by toComp). It is internal to these renders; a
//! public `layer.bounds` query can share it.

use effectcraft_geom::{Mat3, vec2};
use effectcraft_path::{Cap, FillRule, Join, StrokeStyle, fill_coverage, stroke_outline};
use effectcraft_project::{ItemId, Layer, LayerId};
use effectcraft_render::EvalCtx;
use effectcraft_time::Tick;
use kurbo::{Affine, BezPath, Rect, Shape};
use serde_json::{Value, json};

use crate::Session;
use crate::prefs::Prefs;

/// Layers measured for overlays (the rest are left out of the drawing and the JSON).
const MAX_LAYERS: usize = 256;
/// Layers that get a name tag.
const MAX_TAGS: usize = 48;
/// Samples along one motion path.
const MAX_PATH_SAMPLES: i64 = 400;
/// Keyframe squares per motion path.
const MAX_KEYS: usize = 256;
/// Grid lines per direction (a finer grid is skipped).
const MAX_GRID_LINES: f64 = 1000.0;

/// Which overlays to draw.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Overlays {
    pub bounds: bool,
    pub anchor: bool,
    pub motion_path: bool,
    pub safe_margins: bool,
    pub grid: bool,
}

impl Overlays {
    const ACCEPTED: &'static str = "bounds, anchor, motionPath, safeMargins, grid or all (true = bounds + anchor)";

    /// `true` (bounds + anchor), `false`/absent (none), a name, or a list of names.
    pub fn parse(v: Option<&Value>) -> Result<Overlays, String> {
        let mut o = Overlays::default();
        let names: Vec<&Value> = match v {
            None | Some(Value::Null) | Some(Value::Bool(false)) => return Ok(o),
            Some(Value::Bool(true)) => return Ok(Overlays { bounds: true, anchor: true, ..o }),
            Some(Value::Array(a)) => a.iter().collect(),
            Some(s @ Value::String(_)) => vec![s],
            Some(other) => return Err(format!("`overlays` must be true or a list of {}, not {other}", Self::ACCEPTED)),
        };
        for n in names {
            let Some(s) = n.as_str() else { return Err(format!("unknown overlay {n}; use {}", Self::ACCEPTED)) };
            let key: String = s.chars().filter(|c| c.is_ascii_alphanumeric()).collect::<String>().to_ascii_lowercase();
            match key.as_str() {
                "bounds" | "box" | "boxes" | "boundingbox" | "layerbounds" => o.bounds = true,
                "anchor" | "anchors" | "anchorpoint" | "anchorpoints" => o.anchor = true,
                "motionpath" | "motionpaths" | "path" | "paths" | "motion" => o.motion_path = true,
                "safemargins" | "safe" | "safezones" | "titleactionsafe" | "margins" => o.safe_margins = true,
                "grid" => o.grid = true,
                "all" => o = Overlays { bounds: true, anchor: true, motion_path: true, safe_margins: true, grid: true },
                _ => return Err(format!("unknown overlay \"{s}\"; use {}", Self::ACCEPTED)),
            }
        }
        Ok(o)
    }

    pub fn any(&self) -> bool {
        self.bounds || self.anchor || self.motion_path || self.safe_margins || self.grid
    }

    pub fn names(&self) -> Vec<&'static str> {
        [(self.bounds, "bounds"), (self.anchor, "anchor"), (self.motion_path, "motionPath"), (self.safe_margins, "safeMargins"), (self.grid, "grid")]
            .into_iter()
            .filter_map(|(on, n)| on.then_some(n))
            .collect()
    }
}

/// A layer measured in comp space.
#[derive(Clone, Debug, PartialEq)]
pub struct LayerGeom {
    pub id: LayerId,
    /// 1-based index from the top.
    pub index: usize,
    pub name: String,
    /// Label colour (sRGB).
    pub color: [u8; 3],
    /// Content box corners (top left, top right, bottom right, bottom left); `None` for layers
    /// without bounds (cameras, lights, empty text / shapes).
    pub quad: Option<[[f64; 2]; 4]>,
    pub anchor: Option<[f64; 2]>,
    /// The anchor point's comp-space path over the layer's time span (animated layers only).
    pub path: Vec<[f64; 2]>,
    /// The anchor point at each Position / Anchor Point keyframe.
    pub keys: Vec<[f64; 2]>,
}

fn r2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

impl LayerGeom {
    /// Axis-aligned comp-space bounds `[x0, y0, x1, y1]` of the content box.
    pub fn bounds(&self) -> Option<[f64; 4]> {
        let q = self.quad?;
        let xs = q.map(|p| p[0]);
        let ys = q.map(|p| p[1]);
        let min = |a: [f64; 4]| a.into_iter().fold(f64::INFINITY, f64::min);
        let max = |a: [f64; 4]| a.into_iter().fold(f64::NEG_INFINITY, f64::max);
        Some([min(xs), min(ys), max(xs), max(ys)])
    }

    pub fn to_json(&self) -> Value {
        let mut v = json!({"id": self.id.0, "index": self.index, "name": self.name});
        if let Some(b) = self.bounds() {
            v["bounds"] = json!(b.map(r2));
        }
        if let Some(a) = self.anchor {
            v["anchor"] = json!(a.map(r2));
        }
        if !self.keys.is_empty() {
            v["keyPositions"] = json!(self.keys.iter().take(64).map(|k| k.map(r2)).collect::<Vec<_>>());
        }
        v
    }
}

fn finite(p: [f64; 2]) -> Option<[f64; 2]> {
    (p[0].is_finite() && p[1].is_finite()).then_some(p)
}

fn apply(m: &Mat3, x: f64, y: f64) -> Option<[f64; 2]> {
    let q = m.apply(vec2(x, y));
    finite([q.x, q.y])
}

/// The anchor point in comp space at the context time.
fn anchor_at(ctx: &EvalCtx, l: &Layer) -> Option<[f64; 2]> {
    let tr = l.transform()?;
    let a = ctx.v3(l, tr, "anchor", [0.0; 3]);
    let (m, _) = ctx.layer_to_comp(l);
    apply(&m, a[0], a[1])
}

/// Position-like transform properties that move the anchor (Position, its separated X/Y/Z, Anchor
/// Point) and whether any is keyed or driven by an expression.
fn moves(l: &Layer) -> bool {
    l.transform()
        .is_some_and(|tr| tr.props().any(|p| (p.match_id.starts_with("position") || p.match_id == "anchor") && (!p.keys.is_empty() || p.expr.is_some())))
}

/// The layers an overlay shows at `t`: `only`, or every visible layer active at `t` (soloed ones
/// when any is soloed), cameras and lights excluded, at most [`MAX_LAYERS`].
pub fn layer_geometry(s: &Session, cid: ItemId, t: Tick, only: Option<LayerId>, o: &Overlays) -> Vec<LayerGeom> {
    let Some(comp) = s.project.comp(cid) else { return vec![] };
    let ctx = EvalCtx { footage: Some(s.footage.as_ref()), expr: s.expr.as_deref(), ..EvalCtx::new(&s.project, cid, comp, t) };
    let any_solo = comp.layers.iter().any(|l| l.switches.solo && l.has_video());
    let mut out = vec![];
    for (i, l) in comp.layers.iter().enumerate() {
        let wanted = match only {
            Some(id) => l.id == id,
            None => l.switches.video && !l.is_camera() && !l.is_light() && l.is_active_at(t) && (!any_solo || l.switches.solo),
        };
        if !wanted {
            continue;
        }
        if out.len() >= MAX_LAYERS {
            break;
        }
        let (m, _) = ctx.layer_to_comp(l);
        let quad = effectcraft_render::content_bounds(&ctx, l)
            .and_then(|b| Some([apply(&m, b[0], b[1])?, apply(&m, b[2], b[1])?, apply(&m, b[2], b[3])?, apply(&m, b[0], b[3])?]));
        let anchor = anchor_at(&ctx, l);
        let (path, keys) = if o.motion_path && moves(l) { motion_path(&ctx, l) } else { (vec![], vec![]) };
        out.push(LayerGeom { id: l.id, index: i + 1, name: l.name.clone(), color: s.prefs.label_rgb(l.label), quad, anchor, path, keys });
    }
    out
}

/// The anchor's path over the layer's span inside the comp (at most [`MAX_PATH_SAMPLES`] frames)
/// and its position at each Position / Anchor Point keyframe.
fn motion_path(ctx: &EvalCtx, l: &Layer) -> (Vec<[f64; 2]>, Vec<[f64; 2]>) {
    let fr = ctx.comp.frame_rate;
    let t0 = l.in_point.max(Tick::ZERO);
    let t1 = l.out_point.min(ctx.comp.duration);
    let mut path = vec![];
    if t1 > t0 {
        let f0 = fr.frame_at(t0);
        let f1 = fr.frame_at(Tick(t1.0.saturating_sub(1))).max(f0);
        let n = f1.saturating_sub(f0).saturating_add(1);
        let step = (n.saturating_add(MAX_PATH_SAMPLES - 1) / MAX_PATH_SAMPLES).max(1);
        let mut f = f0;
        while f <= f1 {
            if let Some(p) = anchor_at(&ctx.at(fr.tick_of(f)), l) {
                path.push(p);
            }
            if f == f1 {
                break;
            }
            f = f.saturating_add(step).min(f1);
        }
    }
    let mut keys = vec![];
    if let Some(tr) = l.transform() {
        let mut times: Vec<Tick> = tr
            .props()
            .filter(|p| p.match_id.starts_with("position") || p.match_id == "anchor")
            .flat_map(|p| p.keys.iter().map(|k| l.comp_time(k.time)))
            .collect();
        times.sort();
        times.dedup();
        keys = times.into_iter().take(MAX_KEYS).filter_map(|t| anchor_at(&ctx.at(t), l)).collect();
    }
    (path, keys)
}

/// Where the rendered pixels sit in comp space.
pub struct Placement {
    /// Comp-space top-left of the render (the region's corner).
    pub origin: [f64; 2],
    /// Output pixels per comp pixel.
    pub scale: f64,
    /// Comp (or item) frame size.
    pub frame: [f64; 2],
}

impl Placement {
    /// Comp point → output pixel (the renderer rounds the region offset the same way).
    fn out(&self, p: [f64; 2]) -> kurbo::Point {
        kurbo::Point::new(p[0] * self.scale - (self.origin[0] * self.scale).round(), p[1] * self.scale - (self.origin[1] * self.scale).round())
    }
}

/// Paint `paths` (filled, or stroked `width` px wide) in `color` (sRGB + alpha) over straight
/// RGBA, rasterising only their bounding box.
pub(crate) fn paint(rgba: &mut [u8], w: u32, h: u32, paths: &[BezPath], stroke: Option<f64>, color: [u8; 4]) {
    if paths.is_empty() || w == 0 || h == 0 {
        return;
    }
    let shape = match stroke {
        Some(width) => {
            let style = StrokeStyle { width, cap: Cap::Round, join: Join::Round, ..Default::default() };
            match stroke_outline(paths, &style, 1.0) {
                Some(o) => o,
                None => return,
            }
        }
        None => {
            let mut all = BezPath::new();
            for p in paths {
                all.extend(p.elements().iter().copied());
            }
            all
        }
    };
    let b = shape.bounding_box();
    let clip = b.intersect(Rect::new(0.0, 0.0, w as f64, h as f64));
    if !(clip.width() > 0.0 && clip.height() > 0.0) {
        return;
    }
    let (x0, y0) = (clip.x0.floor().max(0.0) as u32, clip.y0.floor().max(0.0) as u32);
    let (x1, y1) = ((clip.x1.ceil() as u32).min(w), (clip.y1.ceil() as u32).min(h));
    let (bw, bh) = (x1.saturating_sub(x0), y1.saturating_sub(y0));
    if bw == 0 || bh == 0 {
        return;
    }
    let mask = fill_coverage(&[shape], &Mat3::translate(vec2(-(x0 as f64), -(y0 as f64))), bw, bh, FillRule::NonZero);
    let a = color[3] as f32 / 255.0;
    for my in 0..bh {
        for mx in 0..bw {
            let Some(&cov) = mask.data.get((my * bw + mx) as usize) else { continue };
            let k = cov * a;
            if k <= 0.0 {
                continue;
            }
            let i = ((y0 + my) as usize * w as usize + (x0 + mx) as usize) * 4;
            if let Some(px) = rgba.get_mut(i..i + 4) {
                over(px, [color[0], color[1], color[2]], k);
            }
        }
    }
}

/// Straight-alpha "over" of `c` at coverage `a` onto one pixel.
fn over(px: &mut [u8], c: [u8; 3], a: f32) {
    let a = a.clamp(0.0, 1.0);
    let da = px.get(3).copied().unwrap_or(0) as f32 / 255.0;
    let oa = a + da * (1.0 - a);
    if oa <= 0.0 {
        return;
    }
    for (i, &ch) in c.iter().enumerate() {
        if let Some(d) = px.get_mut(i) {
            *d = ((ch as f32 * a + *d as f32 * da * (1.0 - a)) / oa).round().clamp(0.0, 255.0) as u8;
        }
    }
    if let Some(d) = px.get_mut(3) {
        *d = (oa * 255.0).round().clamp(0.0, 255.0) as u8;
    }
}

/// Glyph outlines of `text` at `size` px (bundled UI font), top-left at `at`, and their width
/// and height.
pub(crate) fn text_paths(text: &str, size: f64, at: [f64; 2]) -> (Vec<BezPath>, f64, f64) {
    let doc = effectcraft_keyframe::TextDoc { text: text.into(), size, ..Default::default() };
    let lay = effectcraft_text::layout_doc(&doc);
    let b = lay.bounds;
    let (bw, bh) = (b[2] - b[0], b[3] - b[1]);
    if !(bw.is_finite() && bh.is_finite()) || bw <= 0.0 {
        return (vec![], 0.0, 0.0);
    }
    let place = Affine::translate((at[0] - b[0], at[1] - b[1]));
    let paths = lay.glyphs.iter().filter(|g| !g.is_space).map(|g| place * Affine::translate(g.origin.to_vec2()) * g.path.clone()).collect();
    (paths, bw, bh)
}

/// Text on a dark tag whose top-left is `at` (pushed inside the image, and below the tags in
/// `placed` it would cover).
#[allow(clippy::too_many_arguments)]
fn tag(rgba: &mut [u8], w: u32, h: u32, text: &str, at: kurbo::Point, size: f64, color: [u8; 3], placed: &mut Vec<Rect>) {
    let pad = (size * 0.3).round().max(2.0);
    let (_, tw, th) = text_paths(text, size, [0.0, 0.0]);
    if tw <= 0.0 {
        return;
    }
    let (bw, bh) = (tw + 2.0 * pad, th + 2.0 * pad);
    let x = at.x.min(w as f64 - bw).max(0.0);
    let mut y = at.y.min(h as f64 - bh).max(0.0);
    for _ in 0..8 {
        let r = Rect::new(x, y, x + bw, y + bh);
        if !placed.iter().any(|p| {
            let i = p.intersect(r);
            i.width() > 0.0 && i.height() > 0.0
        }) {
            break;
        }
        y += bh + 1.0;
    }
    placed.push(Rect::new(x, y, x + bw, y + bh));
    paint(rgba, w, h, &[Rect::new(x, y, x + bw, y + bh).to_path(0.1)], None, [0, 0, 0, 170]);
    let (paths, _, _) = text_paths(text, size, [x + pad, y + pad]);
    paint(rgba, w, h, &paths, None, [color[0], color[1], color[2], 255]);
}

fn line(a: kurbo::Point, b: kurbo::Point) -> BezPath {
    let mut p = BezPath::new();
    p.move_to(a);
    p.line_to(b);
    p
}

fn polyline(pts: impl IntoIterator<Item = kurbo::Point>, close: bool) -> BezPath {
    let mut p = BezPath::new();
    for (i, q) in pts.into_iter().enumerate() {
        if i == 0 { p.move_to(q) } else { p.line_to(q) }
    }
    if close {
        p.close_path();
    }
    p
}

/// Draw the requested overlays over a render (`w` × `h` straight RGBA) placed by `at`.
pub fn draw(rgba: &mut [u8], w: u32, h: u32, at: &Placement, prefs: &Prefs, geo: &[LayerGeom], o: &Overlays) {
    let [fw, fh] = at.frame;
    if o.grid {
        let g = &prefs.grids;
        let step = g.grid_spacing;
        if step.is_finite() && step > 0.0 && step * at.scale >= 4.0 && fw / step <= MAX_GRID_LINES && fh / step <= MAX_GRID_LINES {
            let mut lines = vec![];
            let mut x = 0.0;
            while x <= fw + 1e-9 {
                lines.push(line(at.out([x, 0.0]), at.out([x, fh])));
                x += step;
            }
            let mut y = 0.0;
            while y <= fh + 1e-9 {
                lines.push(line(at.out([0.0, y]), at.out([fw, y])));
                y += step;
            }
            let c = crate::prefs::parse_hex(&g.grid_color).unwrap_or([122, 156, 255]);
            paint(rgba, w, h, &lines, Some(1.0), [c[0], c[1], c[2], 150]);
        }
    }
    if o.safe_margins {
        let g = &prefs.grids;
        let mut boxes = vec![];
        for pct in [g.action_safe, g.title_safe] {
            let k = (pct.clamp(0.0, 100.0) / 200.0).min(0.49);
            let (dx, dy) = (fw * k, fh * k);
            boxes.push(polyline([at.out([dx, dy]), at.out([fw - dx, dy]), at.out([fw - dx, fh - dy]), at.out([dx, fh - dy])], true));
        }
        // Centre cross.
        let (cx, cy, arm) = (fw / 2.0, fh / 2.0, fw.min(fh) * 0.025);
        boxes.push(line(at.out([cx - arm, cy]), at.out([cx + arm, cy])));
        boxes.push(line(at.out([cx, cy - arm]), at.out([cx, cy + arm])));
        paint(rgba, w, h, &boxes, Some(1.0), [220, 220, 220, 200]);
    }
    if o.motion_path {
        for g in geo {
            if g.path.len() < 2 {
                continue;
            }
            let p = polyline(g.path.iter().map(|q| at.out(*q)), false);
            paint(rgba, w, h, std::slice::from_ref(&p), Some(2.5), [0, 0, 0, 110]);
            paint(rgba, w, h, &[p], Some(1.0), [g.color[0], g.color[1], g.color[2], 255]);
            let squares: Vec<BezPath> = g.keys.iter().map(|k| at.out(*k)).map(|c| Rect::new(c.x - 3.0, c.y - 3.0, c.x + 3.0, c.y + 3.0).to_path(0.1)).collect();
            paint(rgba, w, h, &squares, Some(2.0), [0, 0, 0, 160]);
            paint(rgba, w, h, &squares, None, [g.color[0], g.color[1], g.color[2], 255]);
        }
    }
    if o.bounds {
        for g in geo {
            let Some(q) = g.quad else { continue };
            let p = polyline(q.iter().map(|c| at.out(*c)), true);
            paint(rgba, w, h, std::slice::from_ref(&p), Some(3.0), [0, 0, 0, 110]);
            paint(rgba, w, h, &[p], Some(1.25), [g.color[0], g.color[1], g.color[2], 255]);
        }
    }
    if o.anchor {
        for g in geo {
            let Some(a) = g.anchor else { continue };
            let c = at.out(a);
            let (r, arm) = (4.5, 8.0);
            let shapes = [
                kurbo::Circle::new(c, r).to_path(0.1),
                line(kurbo::Point::new(c.x - arm, c.y), kurbo::Point::new(c.x + arm, c.y)),
                line(kurbo::Point::new(c.x, c.y - arm), kurbo::Point::new(c.x, c.y + arm)),
            ];
            paint(rgba, w, h, &shapes, Some(3.0), [0, 0, 0, 140]);
            paint(rgba, w, h, &shapes, Some(1.25), [g.color[0], g.color[1], g.color[2], 255]);
        }
    }
    if o.bounds {
        // Name tags last, on top, at each box's top-left-most corner.
        let mut placed = vec![];
        for g in geo.iter().take(MAX_TAGS) {
            let Some(q) = g.quad else { continue };
            let Some(corner) = q.iter().map(|c| at.out(*c)).min_by(|a, b| (a.y + a.x * 0.01).total_cmp(&(b.y + b.x * 0.01))) else { continue };
            let name: String = g.name.chars().take(28).collect();
            tag(rgba, w, h, &format!("#{} {name}", g.index), kurbo::Point::new(corner.x, corner.y - 17.0), 11.0, g.color, &mut placed);
        }
    }
}
