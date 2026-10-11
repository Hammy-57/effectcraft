//! Puppet tools in the Composition viewer: the deformed mesh and pins of the selected layers'
//! Puppet effect, click to add a pin (`puppet.addPin`), click / Shift-click a pin to select it
//! (`puppet.selectPins`), drag a pin to move it (`puppet.movePin`), and drag an Advanced or Bend
//! pin's ring to rotate it or its square to scale it (`puppet.setPin`); each drag is one undo step.

use std::sync::Arc;
#[cfg(not(target_arch = "wasm32"))]
use std::sync::Mutex;

use effectcraft_engine::effects::puppet::{self, Mesh, Pin, PinKind};
use effectcraft_engine::geom::vec2 as gv2;
use effectcraft_engine::project::{ItemId, Layer, LayerId};
use effectcraft_engine::render::EvalCtx;
use effectcraft_engine::time::Tick;
use egui::{Color32, Pos2, Stroke};

use super::viewer::{ViewerMap, fx2c};
use crate::EffectcraftApp;

/// Deformed meshes of a layer's Puppet effect: (mesh uid, mesh, deformed vertices, pins).
pub type Overlay = Arc<Vec<(u64, Arc<Mesh>, Vec<[f64; 2]>, Vec<Pin>)>>;

/// Rotation ring radius (screen points) of Advanced and Bend pins.
pub const RING: f32 = 9.0;

/// A pin drawn this frame.
#[derive(Clone, Copy, Debug)]
pub struct PinHit {
    pub layer: LayerId,
    pub pin: u64,
    pub kind: PinKind,
    pub pos: Pos2,
    /// Rotation (°) and scale (1 = 100 %) now, for Advanced and Bend pins.
    pub rotation: f64,
    pub scale: f64,
    /// The scale handle on the ring (Advanced and Bend pins).
    pub handle: Option<Pos2>,
}

/// The part of a pin under the pointer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PinPart {
    /// The pin itself (drag moves Position, Advanced, Starch and Overlap pins).
    Center,
    /// The rotation ring of an Advanced or Bend pin.
    Ring,
    /// The scale handle on that ring.
    Scale,
}

/// The pin part under `p`, topmost pin first.
pub fn hit(hits: &[PinHit], p: Pos2) -> Option<(PinHit, PinPart)> {
    hits.iter().rev().find_map(|h| {
        let d = h.pos.distance(p);
        match h.handle {
            Some(s) if s.distance(p) < 5.0 => Some((*h, PinPart::Scale)),
            Some(_) if d < 6.0 => Some((*h, PinPart::Center)),
            Some(_) if (d - RING).abs() < 4.0 => Some((*h, PinPart::Ring)),
            None if d < 8.0 => Some((*h, PinPart::Center)),
            _ => None,
        }
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct OverlayKey {
    epoch: u64,
    revision: u64,
    comp: u64,
    layer: u64,
    time: i64,
}

impl OverlayKey {
    fn same_layer(self, other: Self) -> bool {
        (self.epoch, self.comp, self.layer) == (other.epoch, other.comp, other.layer)
    }
}

#[derive(Clone)]
struct Cached {
    key: OverlayKey,
    overlay: Overlay,
}

#[cfg(not(target_arch = "wasm32"))]
struct OverlayJob {
    key: OverlayKey,
    source: crate::frames::RenderSource,
    effect: u64,
}

#[cfg(not(target_arch = "wasm32"))]
#[derive(Default)]
struct Mailbox {
    requested: Option<OverlayKey>,
    pending: Option<OverlayJob>,
    completed: Option<Result<Cached, (OverlayKey, String)>>,
    running: bool,
}

#[cfg(not(target_arch = "wasm32"))]
impl Mailbox {
    fn not_admitted(&mut self) {
        self.running = false;
        self.requested = None;
        // An unselected layer may never be visited again. Do not pin its old document while
        // waiting for admission; the next visible frame reconstructs the latest pose instead.
        self.pending = None;
    }

    /// Only the running pose and the latest requested pose retain project snapshots.
    fn request(&mut self, job: OverlayJob) -> bool {
        if self.requested != Some(job.key) {
            self.requested = Some(job.key);
            self.pending = Some(job);
        }
        if self.running || self.pending.is_none() {
            return false;
        }
        self.running = true;
        true
    }
}

/// Native overlay work uses the existing frame pool: no thread per drag or layer, and nothing
/// waits on another pool job. After deselection the last pending solve completes and exits.
#[cfg(not(target_arch = "wasm32"))]
fn run_mailbox(mailbox: &Mutex<Mailbox>, ctx: &egui::Context, mut compute: impl FnMut(&OverlayJob) -> Option<Overlay>) {
    loop {
        let job = {
            let mut mailbox = mailbox.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let Some(job) = mailbox.pending.take() else {
                mailbox.running = false;
                return;
            };
            job
        };
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| compute(&job)));
        let result = match result {
            Ok(Some(overlay)) => Ok(Cached { key: job.key, overlay }),
            Ok(None) => Err((job.key, "Puppet preview input is unavailable; edit a pin or change time to retry.".into())),
            Err(_) => Err((job.key, "Puppet preview failed; edit a pin or change time to retry.".into())),
        };
        mailbox.lock().unwrap_or_else(std::sync::PoisonError::into_inner).completed = Some(result);
        ctx.request_repaint();
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn compute_overlay(job: &OverlayJob) -> Option<Overlay> {
    let cid = ItemId(job.key.comp);
    let comp = job.source.project.comp(cid)?;
    let layer = comp.layer(LayerId(job.key.layer))?;
    let effects = layer.effects()?;
    let index = effects.groups().position(|g| g.uid == job.effect)?;
    let effect = effects.groups().nth(index)?;
    let eval = EvalCtx { project: &job.source.project, comp_id: cid, comp, time: Tick(job.key.time), expr: job.source.expr.as_deref(), footage: None };
    let mut renderer = effectcraft_engine::render::Renderer::new(&job.source.project, &*job.source.footage, Default::default());
    renderer.expr = job.source.expr.as_deref();
    renderer.cache = Some(&job.source.layer_cache);
    let buf = renderer.layer_input(&eval, layer, index)?;
    let params = effectcraft_engine::effects::flatten_params(effect, &mut |p| eval.value(layer, p));
    Some(Arc::new(puppet::overlay(&buf, &params)))
}

/// While the mesh catches up, pins use current property values, so drawing and hit testing
/// follow the pointer immediately. The deformation remains the last exact completed solve.
#[cfg(not(target_arch = "wasm32"))]
fn with_live_pins(overlay: &Overlay, params: &effectcraft_engine::effects::Params) -> Overlay {
    let meshes = puppet::parse(params);
    Arc::new(
        overlay
            .iter()
            .filter_map(|(uid, mesh, vertices, _)| {
                let live = meshes.iter().find(|m| m.uid == *uid)?;
                Some((*uid, mesh.clone(), vertices.clone(), live.pins.clone()))
            })
            .collect(),
    )
}

/// The first overlay is exact. Later native poses solve asynchronously, coalescing drag edits;
/// wasm keeps the synchronous path because there is no native worker pool.
pub fn overlay(app: &mut EffectcraftApp, ctx: &egui::Context, cid: ItemId, layer: &Layer, t: Tick) -> Option<Overlay> {
    let fx = layer.effects()?.groups().find(|g| puppet::is_puppet(g))?;
    let key = OverlayKey { epoch: app.session.epoch, revision: app.session.revision, comp: cid.0, layer: layer.id.0, time: t.0 };
    let id = egui::Id::new(("puppet-overlay", cid.0, layer.id.0));
    let cached = ctx.data(|d| d.get_temp::<Cached>(id)).filter(|c| c.key.same_layer(key));
    if cached.as_ref().is_some_and(|c| c.key == key) {
        return cached.map(|c| c.overlay);
    }
    #[cfg(not(target_arch = "wasm32"))]
    if cached.is_some() {
        let mut cached = cached;
        let worker_id = id.with("worker");
        let mailbox = ctx.data_mut(|d| d.get_temp_mut_or_default::<Arc<Mutex<Mailbox>>>(worker_id).clone());
        let completed = mailbox.lock().unwrap_or_else(std::sync::PoisonError::into_inner).completed.take();
        match completed {
            Some(Ok(done)) if done.key.same_layer(key) => {
                ctx.data_mut(|d| d.insert_temp(id, done.clone()));
                cached = Some(done);
            }
            Some(Err((failed, message))) if failed == key => {
                app.toast = Some((message, ctx.input(|i| i.time)));
            }
            _ => {}
        }
        if cached.as_ref().is_some_and(|c| c.key == key) {
            return cached.map(|c| c.overlay);
        }
        let job = OverlayJob { key, source: app.render_source(), effect: fx.uid };
        let start = mailbox.lock().unwrap_or_else(std::sync::PoisonError::into_inner).request(job);
        if start {
            let worker = mailbox.clone();
            let repaint = ctx.clone();
            if !app.frames.try_spawn_background(move || run_mailbox(&worker, &repaint, compute_overlay)) {
                mailbox.lock().unwrap_or_else(std::sync::PoisonError::into_inner).not_admitted();
            }
        }
        let comp = app.session.project.comp(cid)?;
        let eval = EvalCtx { project: &app.session.project, comp_id: cid, comp, time: t, expr: app.session.expr.as_deref(), footage: None };
        let params = effectcraft_engine::effects::flatten_params(fx, &mut |p| eval.value(layer, p));
        return cached.map(|c| with_live_pins(&c.overlay, &params));
    }
    let (buf, params) = effectcraft_engine::commands::puppet::puppet_eval(&app.session, cid, layer.id, fx.uid, t)?;
    let overlay: Overlay = Arc::new(puppet::overlay(&buf, &params));
    ctx.data_mut(|d| d.insert_temp(id, Cached { key, overlay: overlay.clone() }));
    Some(overlay)
}

/// The layer a Puppet tool press at comp point `cpt` works on, of the selected `layers` with a
/// Puppet effect (stack order): the one whose mesh, as deformed now, is under it (pins go on the
/// deformed shape, also where it reaches past the layer's own bounds), else the one whose art is
/// (a new mesh there). Never a layer without a Puppet effect: the tools stay on the layers being
/// rigged, as in After Effects (#284).
pub fn target(ectx: &EvalCtx, layers: &[(&Layer, Overlay)], cpt: [f64; 2], on_art: impl Fn(&Layer) -> bool) -> Option<LayerId> {
    let on_mesh = |l: &Layer, ov: &Overlay| {
        let Some(inv) = fx2c(ectx, l).inverse() else { return false };
        let p = inv.apply(gv2(cpt[0], cpt[1]));
        ov.iter().any(|(_, mesh, def, _)| puppet::contains(mesh, def, [p.x, p.y]))
    };
    layers.iter().find(|(l, ov)| on_mesh(l, ov)).or_else(|| layers.iter().find(|(l, _)| on_art(l))).map(|(l, _)| l.id)
}

/// Keep a pin's keyframed Position in view in the Timeline after placing, moving or recording the
/// pin in the viewer (#273).
pub fn reveal_pin(app: &mut EffectcraftApp, layer: LayerId, pin: u64) {
    let comp = app.session.active_comp();
    let pos = comp.and_then(|c| c.layer(layer)?.props.find_group(pin)?.get("position")).filter(|p| p.is_animated()).map(|p| p.uid);
    if let Some(pos) = pos {
        super::timeline::keep_in_view(app, layer, pos);
    }
}

/// Where a pin sits now (effect space): its Position, or (Bend / Starch / Overlap pins) its rest
/// point carried along by the deformation.
pub fn pin_position(mesh: &Mesh, def: &[[f64; 2]], p: &Pin) -> [f64; 2] {
    if p.kind.moves() {
        return p.position;
    }
    let v = (0..mesh.verts.len()).min_by(|a, b| {
        let da = (mesh.verts[*a][0] - p.rest[0]).hypot(mesh.verts[*a][1] - p.rest[1]);
        let db = (mesh.verts[*b][0] - p.rest[0]).hypot(mesh.verts[*b][1] - p.rest[1]);
        da.total_cmp(&db)
    });
    match v {
        Some(v) => [def[v][0] + p.rest[0] - mesh.verts[v][0], def[v][1] + p.rest[1] - mesh.verts[v][1]],
        None => p.rest,
    }
}

/// Pin colours by kind: Position yellow, Starch red, Overlap blue, Advanced blue-green, Bend
/// orange-brown.
pub fn pin_color(kind: PinKind) -> Color32 {
    match kind {
        PinKind::Position => Color32::from_rgb(0xf0, 0xd0, 0x40),
        PinKind::Starch => Color32::from_rgb(0xe0, 0x50, 0x50),
        PinKind::Overlap => Color32::from_rgb(0x40, 0x90, 0xf0),
        PinKind::Advanced => Color32::from_rgb(0x30, 0xc8, 0xb4),
        PinKind::Bend => Color32::from_rgb(0xc8, 0x82, 0x3c),
    }
}

/// Draw the mesh (when `show_mesh`) and pins of `layer` (the pin under `hover` is drawn larger);
/// returns pin hit targets.
#[allow(clippy::too_many_arguments)]
pub fn draw(
    painter: &egui::Painter,
    map: &ViewerMap,
    ectx: &EvalCtx,
    layer: &Layer,
    ov: &Overlay,
    show_mesh: bool,
    selected: &[u64],
    hover: Option<Pos2>,
) -> Vec<PinHit> {
    let m = fx2c(ectx, layer);
    let scr = |q: [f64; 2]| {
        let c = m.apply(gv2(q[0], q[1]));
        map.to_screen([c.x, c.y])
    };
    let mut hits = vec![];
    for (_, mesh, def, pins) in ov.iter() {
        if show_mesh {
            let st = Stroke::new(1.0, Color32::from_rgba_unmultiplied(0xf0, 0xd0, 0x40, 110));
            for t in &mesh.tris {
                let (a, b, c) = (scr(def[t[0]]), scr(def[t[1]]), scr(def[t[2]]));
                painter.line_segment([a, b], st);
                painter.line_segment([b, c], st);
                painter.line_segment([c, a], st);
            }
        }
        for pn in pins {
            let at = pin_position(mesh, def, pn);
            let pos = scr(at);
            let sel = selected.contains(&pn.uid);
            let col = pin_color(pn.kind);
            if matches!(pn.kind, PinKind::Starch | PinKind::Overlap) {
                // Extent ring (layer pixels → screen).
                let r = (scr([pn.rest[0] + pn.extent, pn.rest[1]]) - scr(pn.rest)).length();
                painter.circle_stroke(pos, r.max(2.0), Stroke::new(1.0, col.gamma_multiply(0.6)));
            }
            // Advanced and Bend pins: the rotation ring with a square scale handle at the pin's
            // rotation (the layer's own orientation on screen).
            let mut handle = None;
            if matches!(pn.kind, PinKind::Bend | PinKind::Advanced) {
                painter.circle_stroke(pos, RING, Stroke::new(1.5, col));
                let (s, c) = pn.rotation.to_radians().sin_cos();
                let dir = (scr([at[0] + c, at[1] + s]) - pos).normalized();
                if dir.x.is_finite() && dir.y.is_finite() {
                    let hp = pos + dir * RING;
                    painter.rect_filled(egui::Rect::from_center_size(hp, egui::vec2(5.0, 5.0)), 0.0, col);
                    painter.rect_stroke(
                        egui::Rect::from_center_size(hp, egui::vec2(5.0, 5.0)),
                        0.0,
                        Stroke::new(1.0, Color32::BLACK),
                        egui::StrokeKind::Outside,
                    );
                    handle = Some(hp);
                }
            }
            // Pins grow under the pointer.
            // Selected pins are filled with their colour and ringed in white; unselected pins are
            // hollow (dark centre, coloured outline), like selected and unselected mask vertices.
            // Drawing only: hit-testing uses `pos` alone.
            let r = if sel { 5.5 } else { 4.5 } + if hover.is_some_and(|h| h.distance(pos) < 8.0) { 1.5 } else { 0.0 };
            if sel {
                painter.circle_filled(pos, r, col);
                painter.circle_stroke(pos, r, Stroke::new(1.0, Color32::BLACK));
                painter.circle_stroke(pos, r + 2.5, Stroke::new(1.5, Color32::WHITE));
            } else {
                painter.circle_filled(pos, r, Color32::from_black_alpha(160));
                painter.circle_stroke(pos, r, Stroke::new(1.5, col));
            }
            hits.push(PinHit { layer: layer.id, pin: pn.uid, kind: pn.kind, pos, rotation: pn.rotation, scale: pn.scale, handle });
        }
    }
    hits
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod async_tests {
    use super::*;
    use effectcraft_engine::Session;
    use serde_json::json;

    fn job(revision: u64, epoch: u64) -> OverlayJob {
        OverlayJob {
            key: OverlayKey { epoch, revision, comp: 1, layer: 2, time: 0 },
            source: crate::frames::RenderSource {
                project: Arc::default(),
                footage: Arc::new(effectcraft_engine::render::NoFootage),
                expr: None,
                layer_cache: Arc::default(),
                gpu: None,
                gpu_display: false,
                disk: None,
            },
            effect: 3,
        }
    }

    #[test]
    fn rejected_admission_retries_the_same_pending_pose() {
        let mut mailbox = Mailbox::default();
        let first = job(1, 0);
        let old_project = Arc::downgrade(&first.source.project);
        assert!(mailbox.request(first));
        // Another layer owns the shared render permit; no worker was submitted.
        mailbox.not_admitted();
        assert!(old_project.upgrade().is_none(), "deselection must not retain a rejected document snapshot");
        assert!(mailbox.request(job(1, 0)), "retry pending work when the permit becomes free");
        assert_eq!(mailbox.pending.as_ref().unwrap().key.revision, 1);
        assert!(!mailbox.request(job(2, 0)), "coalesce while the retried worker runs");
        assert_eq!(mailbox.pending.as_ref().unwrap().key.revision, 2);
    }

    #[test]
    fn moving_poses_coalesce_to_one_pending_snapshot_and_worker_exits() {
        let mailbox = Mutex::new(Mailbox::default());
        assert!(mailbox.lock().unwrap().request(job(1, 0)));
        let mut solved = vec![];
        run_mailbox(&mailbox, &egui::Context::default(), |j| {
            solved.push(j.key.revision);
            if j.key.revision == 1 {
                for revision in 2..=100 {
                    assert!(!mailbox.lock().unwrap().request(job(revision, 0)), "no second worker");
                }
            }
            Some(Arc::new(vec![]))
        });
        assert_eq!(solved, [1, 100]);
        let mailbox = mailbox.lock().unwrap();
        assert!(!mailbox.running);
        assert!(mailbox.pending.is_none());
        assert_eq!(mailbox.completed.as_ref().unwrap().as_ref().ok().unwrap().key.revision, 100);
    }

    #[test]
    fn failed_overlay_releases_worker_and_next_edit_retries() {
        let mailbox = Mutex::new(Mailbox::default());
        assert!(mailbox.lock().unwrap().request(job(1, 0)));
        run_mailbox(&mailbox, &egui::Context::default(), |_| panic!("synthetic solver failure"));
        {
            let mut state = mailbox.lock().unwrap();
            assert!(!state.running);
            assert!(state.completed.as_ref().unwrap().is_err());
            assert!(!state.request(job(1, 0)), "no hot retry of the failing pose");
            assert!(state.request(job(2, 0)), "another edit can start a worker");
        }
        run_mailbox(&mailbox, &egui::Context::default(), |_| Some(Arc::new(vec![])));
        assert!(mailbox.lock().unwrap().completed.as_ref().unwrap().is_ok());
        assert!(!job(2, 0).key.same_layer(job(2, 1).key), "ids reused in another document cannot reuse overlays");
    }

    #[test]
    fn pins_follow_edits_while_mesh_worker_is_busy_then_finish_exactly() {
        let mut session = Session::default();
        let cid = ItemId(session.execute("comp.new", json!({"width": 64, "height": 64, "duration": 1})).unwrap()["comp"].as_u64().unwrap());
        let lid = LayerId(session.execute("layer.newSolid", json!({"width": 48, "height": 48, "color": "#ff0000"})).unwrap()["layer"].as_u64().unwrap());
        session.execute("puppet.addPin", json!({"layer": lid.0, "position": [4, 4]})).unwrap();
        let pin = session.execute("puppet.addPin", json!({"layer": lid.0, "position": [40, 40]})).unwrap()["pin"].as_u64().unwrap();
        let mut app = EffectcraftApp::new(session);
        let ctx = egui::Context::default();
        let layer = app.session.project.comp(cid).unwrap().layer(lid).unwrap().clone();
        let first = overlay(&mut app, &ctx, cid, &layer, Tick::ZERO).unwrap();
        let id = egui::Id::new(("puppet-overlay", cid.0, lid.0));
        let mailbox = Arc::new(Mutex::new(Mailbox { running: true, ..Default::default() }));
        ctx.data_mut(|d| d.insert_temp(id.with("worker"), mailbox.clone()));
        app.session.execute("puppet.movePin", json!({"layer": lid.0, "pin": pin, "position": [58, 30]})).unwrap();
        let layer = app.session.project.comp(cid).unwrap().layer(lid).unwrap().clone();
        let began = std::time::Instant::now();
        let live = overlay(&mut app, &ctx, cid, &layer, Tick::ZERO).unwrap();
        eprintln!("busy-worker live overlay update: {:.3} ms", began.elapsed().as_secs_f64() * 1000.0);
        let moved = live.iter().flat_map(|(_, _, _, pins)| pins).find(|p| p.uid == pin).unwrap();
        assert_eq!(moved.position, [58.0, 30.0], "pin drawing and hits use the latest edit without waiting");
        assert_eq!(live[0].2, first[0].2, "last completed geometry stays until replacement is ready");
        assert!(mailbox.lock().unwrap().pending.is_some(), "the new deformation was queued");
        run_mailbox(&mailbox, &ctx, compute_overlay);
        let done = overlay(&mut app, &ctx, cid, &layer, Tick::ZERO).unwrap();
        let effect = layer.effects().unwrap().groups().find(|g| puppet::is_puppet(g)).unwrap().uid;
        let (buf, params) = effectcraft_engine::commands::puppet::puppet_eval(&app.session, cid, lid, effect, Tick::ZERO).unwrap();
        assert_eq!(*done, puppet::overlay(&buf, &params), "settled overlay is the exact final solve");
        assert_ne!(done[0].2, first[0].2);

        let old = ctx.data(|d| d.get_temp::<Cached>(id)).unwrap();
        app.session.replace_project((*app.session.project).clone(), None);
        let layer = app.session.project.comp(cid).unwrap().layer(lid).unwrap().clone();
        assert!(!overlay(&mut app, &ctx, cid, &layer, Tick::ZERO).unwrap().is_empty());
        {
            let mut worker = mailbox.lock().unwrap();
            worker.running = true;
            worker.completed = Some(Ok(Cached { key: old.key, overlay: Arc::new(vec![]) }));
        }
        app.session.execute("puppet.movePin", json!({"layer": lid.0, "pin": pin, "position": [55, 32]})).unwrap();
        let layer = app.session.project.comp(cid).unwrap().layer(lid).unwrap().clone();
        assert!(!overlay(&mut app, &ctx, cid, &layer, Tick::ZERO).unwrap().is_empty(), "late completion from a replaced document is discarded");
    }
}
