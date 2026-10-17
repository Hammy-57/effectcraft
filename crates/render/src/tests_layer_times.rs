//! [`LayerTimes`]: every layer that renders on a frame gets its time (the Timeline's Render Time
//! pane), a precomp's nested layers under their own comp and the precomp layer including them.

use effectcraft_color::Label;
use effectcraft_project::build;
use effectcraft_project::{Comp, ItemId, ItemKind, Layer, LayerId, LayerSource, Project, Solid};
use effectcraft_time::{FrameRate, Tick};

use crate::{LayerCache, LayerTimes, NoFootage, RenderOpts, Renderer};

fn solid(p: &mut Project, comp: &Comp, color: [f32; 3], w: u32, h: u32) -> Layer {
    let sid = p.add_item("Solid", Label::Red, None, ItemKind::Solid(Solid { color, width: w, height: h, pixel_aspect: 1.0 }));
    build::layer(p, comp, "Solid", LayerSource::Solid { item: sid }, (w, h), None)
}

/// A comp of a 2D solid, a precomp layer, a collapsed precomp layer and a run of two 3D solids:
/// (project, comp, [(comp, layer)] of every layer that draws).
fn scene() -> (Project, ItemId, Vec<(ItemId, LayerId)>) {
    let mut p = Project::default();
    let comp = Comp::new(200, 100, FrameRate::FPS_30, Tick::from_seconds_f64(2.0));
    let cid = p.add_item("Main", Label::Sandstone, None, ItemKind::Comp(comp.clone().into()));
    let mut drawn = vec![];
    let mut nest = |p: &mut Project, collapse: bool| {
        let inner = Comp::new(100, 100, FrameRate::FPS_30, Tick::from_seconds_f64(2.0));
        let iid = p.add_item("Inner", Label::Sandstone, None, ItemKind::Comp(inner.clone().into()));
        let s = solid(p, &inner, [1.0, 0.5, 0.0], 60, 60);
        drawn.push((iid, s.id));
        p.comp_mut(iid).unwrap().layers.push(s);
        let mut pre = build::layer(p, &comp, "Inner", LayerSource::Comp { item: iid }, (100, 100), None);
        pre.switches.collapse = collapse;
        drawn.push((cid, pre.id));
        pre
    };
    let pre = nest(&mut p, false);
    let collapsed = nest(&mut p, true);
    let mut layers = vec![pre, collapsed];
    for z in [0.0, 20.0] {
        let mut l = solid(&mut p, &comp, [0.2, 0.4, z as f32 / 20.0], 80, 40);
        l.switches.three_d = true;
        l.props.prop_mut("transform/position").unwrap().value = effectcraft_keyframe::Value::Vec3([100.0, 50.0, z]);
        layers.push(l);
    }
    layers.push(solid(&mut p, &comp, [0.1, 0.1, 0.1], 200, 100));
    for l in &layers[2..] {
        drawn.push((cid, l.id));
    }
    for l in layers {
        p.comp_mut(cid).unwrap().layers.push(l);
    }
    (p, cid, drawn)
}

#[test]
fn every_drawn_layer_has_a_time_and_a_precomp_includes_its_layers() {
    let (p, cid, drawn) = scene();
    for cache in [None, Some(LayerCache::default())] {
        let times = LayerTimes::default();
        let mut r = Renderer::new(&p, &NoFootage, RenderOpts::default());
        r.cache = cache.as_ref();
        r.times = Some(&times);
        let img = r.comp_frame(cid, Tick::ZERO);
        assert!(img.get(100, 50)[3] > 0.99);
        let m = times.into_map();
        for key in &drawn {
            assert!(m.get(key).is_some_and(|ms| ms.is_finite() && *ms >= 0.0), "no time for {key:?} (cache {}): {m:?}", cache.is_some());
        }
        // The precomp layer's time includes the layer drawn inside it.
        let (inner, nested) = drawn[0];
        let pre = drawn[1];
        assert!(m[&pre] >= m[&(inner, nested)], "{m:?}");
    }
    // Untimed renders time nothing (and render the same).
    let r = Renderer::new(&p, &NoFootage, RenderOpts::default());
    assert!(r.times.is_none());
}

#[test]
fn a_3d_run_splits_its_shared_time_between_its_layers() {
    let (p, cid, drawn) = scene();
    let run: Vec<_> = drawn.iter().filter(|(c, l)| *c == cid && p.comp(cid).unwrap().layer(*l).is_some_and(|l| l.is_3d())).copied().collect();
    assert_eq!(run.len(), 2);
    let times = LayerTimes::default();
    let mut r = Renderer::new(&p, &NoFootage, RenderOpts::default());
    r.times = Some(&times);
    let comp = p.comp(cid).unwrap();
    let ctx = crate::EvalCtx::new(&p, cid, comp, Tick::ZERO);
    let layers: Vec<&Layer> = run.iter().filter_map(|(_, l)| comp.layer(*l)).collect();
    r.timed_run(&ctx, &layers, || std::thread::sleep(std::time::Duration::from_millis(20)));
    let m = times.into_map();
    for key in &run {
        assert!(m[key] >= 9.0, "half of the run's 20 ms each: {m:?}");
    }
}
