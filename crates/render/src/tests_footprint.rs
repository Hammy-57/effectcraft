//! Per-layer work clipped to what the layer can reach: transforms evaluated once per draw
//! (motion blur), and the results identical to the unshared, full-canvas paths.

use effectcraft_color::Label;
use effectcraft_keyframe::{Keyframe, Value};
use effectcraft_project::build;
use effectcraft_project::{Comp, ItemId, ItemKind, Layer, LayerSource, Project, Solid};
use effectcraft_time::{FrameRate, Tick};

use crate::motion_blur::MotionMemo;
use crate::{Buf, Image, NoFootage, RenderOpts, Renderer};

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
