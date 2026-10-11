//! Per-layer work clipped to what the layer can reach: transforms evaluated once per draw
//! (motion blur), and the results identical to the unshared, full-canvas paths.

use std::cell::Cell;

use effectcraft_color::{BlendMode, Label};
use effectcraft_keyframe::{Keyframe, Value};
use effectcraft_project::build::{self, Ids};
use effectcraft_project::styles::{self as st, GROUP};
use effectcraft_project::{BitDepth, Comp, ItemId, ItemKind, Layer, LayerSource, MatteKind, Project, Solid, TrackMatte};
use effectcraft_time::{FrameRate, Tick};

use crate::motion_blur::MotionMemo;
use crate::{Buf, Image, NoFootage, RenderOpts, Renderer};

thread_local! {
    /// Draw every temporary over the whole canvas (the path before windows).
    static FULL_WINDOWS: Cell<bool> = const { Cell::new(false) };
}

/// Whether this thread draws whole-canvas temporaries (see [`crate::Renderer::reach`]).
pub(crate) fn full_windows() -> bool {
    FULL_WINDOWS.with(Cell::get)
}

fn setup() -> (Project, ItemId, Comp) {
    let mut p = Project::default();
    p.settings.bit_depth = effectcraft_project::BitDepth::Bpc32;
    let mut comp = Comp::new(200, 100, FrameRate::FPS_30, Tick::from_seconds_f64(2.0));
    comp.enable_motion_blur = true;
    let cid = p.add_item("Comp", Label::Sandstone, None, ItemKind::Comp(comp.clone().into()));
    (p, cid, comp)
}

fn solid(p: &mut Project, comp: &Comp, color: [f32; 3], w: u32, h: u32) -> Layer {
    let sid = p.add_item("Solid", Label::Red, None, ItemKind::Solid(Solid { color, width: w, height: h, pixel_aspect: 1.0 }));
    build::layer(p, comp, "Solid", LayerSource::Solid { item: sid }, (w, h), None)
}

/// Position keyframes from `a` (0 s) to `b` (1 s).
fn moving(l: &mut Layer, a: [f64; 2], b: [f64; 2]) {
    let pos = l.props.prop_mut("transform/position").unwrap();
    pos.keys = vec![Keyframe::new(Tick::ZERO, Value::Vec3([a[0], a[1], 0.0])), Keyframe::new(Tick::from_seconds_f64(1.0), Value::Vec3([b[0], b[1], 0.0]))];
}

/// A motion-blurred layer's draw evaluates each of its matrices once: the sample-count probes,
/// the sub-samples, the changed region and other buffers (styles) all share them, and they equal
/// the unshared ones.
#[test]
fn motion_blur_matrices_are_evaluated_once_per_draw() {
    let (mut p, cid, comp) = setup();
    let mut l = solid(&mut p, &comp, [1.0, 0.0, 0.0], 20, 20);
    moving(&mut l, [20.0, 50.0], [180.0, 50.0]);
    l.switches.motion_blur = true;
    p.comp_mut(cid).unwrap().layers.push(l);
    let r = Renderer::new(&p, &NoFootage, RenderOpts::default());
    let c = p.comp(cid).unwrap();
    let ctx = r.ctx(cid, c, Tick::from_seconds_f64(0.5));
    let layer = c.layers.iter().next().unwrap();
    let buf = Buf { img: Image::new(20, 20), offset: [0.0; 2], scale: 1.0 };
    let mo = MotionMemo::default();
    let pl = r.placement_memo(&ctx, layer, &buf, &mo);
    assert!(pl.matrices.len() > 8, "{} samples", pl.matrices.len());
    assert_eq!(pl.matrices, r.placement(&ctx, layer, &buf).matrices);
    let n = mo.evaluated();
    assert!(n <= 8 + pl.matrices.len(), "{n} evaluations");
    let _ = r.footprint(&ctx, layer, &buf, 1.0, false, &mo);
    let shadow = Buf { img: Image::new(30, 26), offset: [5.0, 3.0], scale: 1.0 };
    let ps = r.placement_memo(&ctx, layer, &shadow, &mo);
    assert_eq!(ps.matrices, r.placement(&ctx, layer, &shadow).matrices);
    assert_eq!(mo.evaluated(), n, "the changed region and another buffer evaluate nothing new");
}

fn render(p: &Project, cid: ItemId, t: f64, full: bool) -> Image {
    FULL_WINDOWS.with(|f| f.set(full));
    let img = Renderer::new(p, &NoFootage, RenderOpts::default()).comp_frame(cid, Tick::from_seconds_f64(t));
    FULL_WINDOWS.with(|f| f.set(false));
    img
}

/// Renders `scene` at 8 and 32 bpc, with windowed and with whole-canvas temporaries: identical.
fn assert_windows_match(what: &str, scene: impl Fn(&mut Project, &Comp) -> Vec<Layer>) {
    for depth in [BitDepth::Bpc8, BitDepth::Bpc32] {
        let (mut p, cid, comp) = setup();
        p.settings.bit_depth = depth;
        let layers = scene(&mut p, &comp);
        p.comp_mut(cid).unwrap().layers = layers.into();
        for t in [0.0, 0.25, 0.5] {
            let (a, b) = (render(&p, cid, t, false), render(&p, cid, t, true));
            assert!(a.data.iter().any(|p| p[3] > 0.0), "{what}: something is drawn");
            assert!(a == b, "{what} {depth:?} t {t}: windowed temporaries differ from whole-canvas ones");
        }
    }
}

/// A 200×100 background gradient-ish stack: two solids.
fn background(p: &mut Project, comp: &Comp) -> Vec<Layer> {
    let mut a = solid(p, comp, [0.2, 0.5, 0.8], 120, 100);
    a.props.prop_mut("transform/position").unwrap().value = Value::Vec3([60.0, 50.0, 0.0]);
    a.props.prop_mut("transform/opacity").unwrap().value = Value::Scalar(70.0);
    let b = solid(p, comp, [0.9, 0.4, 0.1], 200, 100);
    vec![a, b]
}

/// A small rotated, half-transparent layer that moves (and is motion blurred when `mb`).
fn small(p: &mut Project, comp: &Comp, mb: bool) -> Layer {
    let mut l = solid(p, comp, [0.3, 0.9, 0.6], 40, 30);
    moving(&mut l, [40.0, 40.0], [160.0, 60.0]);
    l.props.prop_mut("transform/rotation").unwrap().value = Value::Scalar(17.0);
    l.props.prop_mut("transform/opacity").unwrap().value = Value::Scalar(80.0);
    l.switches.motion_blur = mb;
    l
}

fn matte_layer(p: &mut Project, comp: &Comp) -> Layer {
    let mut m = solid(p, comp, [0.8, 0.8, 0.8], 70, 50);
    m.props.prop_mut("transform/position").unwrap().value = Value::Vec3([90.0, 45.0, 0.0]);
    m.props.prop_mut("transform/rotation").unwrap().value = Value::Scalar(-8.0);
    m.switches.video = false;
    m
}

fn style(p: &mut Project, comp: &Comp, l: &mut Layer, name: &str, vals: &[(&str, Value)]) {
    let mut next = p.next_id;
    st::add_style(l, &mut Ids(&mut next), name, &comp.global_light, true).unwrap();
    p.next_id = next;
    for (k, v) in vals {
        l.props.prop_mut(&format!("{GROUP}/{k}")).unwrap().value = v.clone();
    }
}

#[test]
fn track_mattes_draw_only_the_window_they_reach() {
    for (kind, mode, mb) in [
        (MatteKind::Alpha, BlendMode::Normal, false),
        (MatteKind::LumaInverted, BlendMode::Dissolve, false),
        (MatteKind::AlphaInverted, BlendMode::Screen, true),
        (MatteKind::Luma, BlendMode::StencilAlpha, false),
    ] {
        assert_windows_match(&format!("matte {kind:?} {mode:?} mb {mb}"), |p, c| {
            let m = matte_layer(p, c);
            let mut l = small(p, c, mb);
            l.track_matte = Some(TrackMatte { layer: m.id, kind });
            l.blend_mode = mode;
            let mut v = vec![m, l];
            v.extend(background(p, c));
            v
        });
    }
}

#[test]
fn preserve_transparency_draws_only_the_window_it_reaches() {
    for (mode, mb) in [(BlendMode::Normal, false), (BlendMode::Multiply, true), (BlendMode::DancingDissolve, false)] {
        assert_windows_match(&format!("preserve {mode:?} mb {mb}"), |p, c| {
            let mut l = small(p, c, mb);
            l.preserve_transparency = true;
            l.blend_mode = mode;
            let mut v = vec![l];
            v.extend(background(p, c));
            v
        });
    }
}

#[test]
fn styled_layers_draw_only_the_window_they_reach() {
    let shadow = [("dropShadow/distance", Value::Scalar(12.0)), ("dropShadow/size", Value::Scalar(6.0))];
    for (matte, knockout, mode, mb) in [
        (false, 0, BlendMode::Normal, false),
        (false, 2, BlendMode::Overlay, true),
        (true, 0, BlendMode::Normal, false),
        (false, 0, BlendMode::StencilAlpha, false),
    ] {
        assert_windows_match(&format!("styles matte {matte} knockout {knockout} {mode:?} mb {mb}"), |p, c| {
            let mut l = small(p, c, mb);
            style(p, c, &mut l, "dropShadow", &shadow);
            style(p, c, &mut l, "outerGlow", &[]);
            if knockout > 0 {
                l.props.prop_mut(&format!("{GROUP}/blendingOptions/advancedBlending/fillOpacity")).unwrap().value = Value::Scalar(40.0);
                l.props.prop_mut(&format!("{GROUP}/blendingOptions/advancedBlending/knockout")).unwrap().value = Value::Enum(knockout);
            }
            l.blend_mode = mode;
            let mut v = vec![];
            if matte {
                let m = matte_layer(p, c);
                l.track_matte = Some(TrackMatte { layer: m.id, kind: MatteKind::Alpha });
                v.push(m);
            }
            v.push(l);
            v.extend(background(p, c));
            v
        });
    }
}

#[test]
fn adjustment_layers_blend_only_the_window_they_reach() {
    for (matte, mode) in [(false, BlendMode::Normal), (true, BlendMode::Screen)] {
        assert_windows_match(&format!("adjustment matte {matte} {mode:?}"), |p, c| {
            let mut adj = small(p, c, false);
            adj.switches.adjustment = true;
            adj.blend_mode = mode;
            let spec = effectcraft_effects::find("ec.blur.gaussian").unwrap();
            let mut next = p.next_id;
            let mut g = effectcraft_effects::instantiate(spec, &mut Ids(&mut next), spec.name, [40.0, 30.0]);
            p.next_id = next;
            g.prop_mut("blurriness").unwrap().value = Value::Scalar(8.0);
            adj.props.sub_mut("effects").unwrap().children.push(g.into());
            let mut v = vec![];
            if matte {
                let m = matte_layer(p, c);
                adj.track_matte = Some(TrackMatte { layer: m.id, kind: MatteKind::Luma });
                v.push(m);
            }
            v.push(adj);
            v.extend(background(p, c));
            v
        });
    }
}

#[test]
fn motion_blur_accumulates_only_the_window_it_reaches() {
    for mode in [BlendMode::Normal, BlendMode::Dissolve, BlendMode::Add, BlendMode::SilhouetteAlpha] {
        assert_windows_match(&format!("motion blur {mode:?}"), |p, c| {
            let mut l = small(p, c, true);
            l.blend_mode = mode;
            let mut v = vec![l];
            v.extend(background(p, c));
            v
        });
    }
}

/// Layers drawn this frame (the per-layer timing sink records each one that renders).
fn drawn(p: &Project, cid: ItemId, t: f64) -> (Image, Vec<String>) {
    let prof = std::sync::Mutex::new(vec![]);
    let mut r = Renderer::new(p, &NoFootage, RenderOpts::default());
    r.profile = Some(&prof);
    let img = r.comp_frame(cid, Tick::from_seconds_f64(t));
    let names = prof.into_inner().unwrap().into_iter().map(|t| t.layer).collect();
    (img, names)
}

/// Off-comp layers are skipped before their source renders (an effect that doesn't grow them
/// into the comp included); ones that reach the comp by a pixel or grow into it through
/// effects are still drawn.
#[test]
fn layers_off_the_comp_are_not_rendered() {
    let (mut p, cid, comp) = setup();
    let named = |p: &mut Project, name: &str, pos: [f64; 2], f: &dyn Fn(&mut Project, &mut Layer)| {
        let mut l = solid(p, &comp, [0.4, 0.6, 0.8], 40, 30);
        l.name = name.into();
        l.props.prop_mut("transform/position").unwrap().value = Value::Vec3([pos[0], pos[1], 0.0]);
        f(p, &mut l);
        l
    };
    let none = |_: &mut Project, _: &mut Layer| {};
    let off = named(&mut p, "off", [400.0, 50.0], &none);
    let off_multiply = named(&mut p, "off multiply", [-300.0, -200.0], &|_, l| l.blend_mode = BlendMode::Multiply);
    // Its right edge (x 1) is just inside the comp.
    let edge = named(&mut p, "edge", [-19.0, 50.0], &none);
    let blur = |blurriness: f64| {
        move |p: &mut Project, l: &mut Layer| {
            let spec = effectcraft_effects::find("ec.blur.gaussian").unwrap();
            let mut next = p.next_id;
            let mut g = effectcraft_effects::instantiate(spec, &mut Ids(&mut next), spec.name, [40.0, 30.0]);
            p.next_id = next;
            g.prop_mut("blurriness").unwrap().value = Value::Scalar(blurriness);
            l.props.sub_mut("effects").unwrap().children.push(g.into());
        }
    };
    // A blur that doesn't reach the comp (none at all) is culled like the plain layer; one whose
    // spread reaches it from as far away is drawn.
    let fx = named(&mut p, "off fx", [400.0, 50.0], &blur(0.0));
    let reach = named(&mut p, "fx reach", [400.0, 50.0], &blur(400.0));
    let stencil = named(&mut p, "off stencil", [400.0, 50.0], &|_, l| l.blend_mode = BlendMode::StencilAlpha);
    let mut adj = named(&mut p, "adjustment", [100.0, 50.0], &|_, l| l.props.prop_mut("transform/opacity").unwrap().value = Value::Scalar(0.0));
    adj.switches.adjustment = true;
    let bg = solid(&mut p, &comp, [0.9, 0.4, 0.1], 200, 100);
    let layers = vec![adj, off, off_multiply, edge, fx, reach.clone(), bg.clone()];
    p.comp_mut(cid).unwrap().layers = layers.into();
    let (img, names) = drawn(&p, cid, 0.0);
    assert!(!names.iter().any(|n| n == "off" || n == "off multiply" || n == "off fx"), "off-comp layers render nothing: {names:?}");
    assert!(names.iter().any(|n| n == "edge") && names.iter().any(|n| n == "fx reach"), "{names:?}");
    assert!(img.get(0, 50)[2] > 0.7, "the edge layer's last column: {:?}", img.get(0, 50));
    // The same pixels as the visible layers alone (opacity-0 adjustment layer included).
    p.comp_mut(cid).unwrap().layers = vec![named(&mut p, "edge", [-19.0, 50.0], &none), reach, bg.clone()].into();
    let (only, _) = drawn(&p, cid, 0.0);
    assert!(img == only, "culled layers changed nothing");
    // A stencil layer changes the comp outside itself: never culled.
    p.comp_mut(cid).unwrap().layers = vec![stencil, bg].into();
    let (_, names) = drawn(&p, cid, 0.0);
    assert!(names.iter().any(|n| n == "off stencil"), "{names:?}");
}
