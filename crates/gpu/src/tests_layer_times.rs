//! The GPU compositor times every layer it draws for the Timeline's Render Time pane, as the CPU
//! one does (`effectcraft_render::LayerTimes`): before, only the CPU's per-layer sink existed and
//! no GPU frame had times.

use effectcraft_color::Label;
use effectcraft_project::build;
use effectcraft_project::{Comp, ItemKind, Layer, LayerSource, Project, Solid};
use effectcraft_render::{Backend, LayerCache, LayerTimes, NoFootage, RenderOpts, Renderer};
use effectcraft_time::{FrameRate, Tick};

fn solid(p: &mut Project, comp: &Comp, color: [f32; 3], w: u32, h: u32) -> Layer {
    let sid = p.add_item("Solid", Label::Red, None, ItemKind::Solid(Solid { color, width: w, height: h, pixel_aspect: 1.0 }));
    build::layer(p, comp, "Solid", LayerSource::Solid { item: sid }, (w, h), None)
}

#[test]
fn gpu_frames_time_every_layer_and_the_layers_inside_precomps() {
    crate::tests::hold_gpu_lock();
    let Some(g) = crate::tests::gpu() else { return };
    let mut p = Project::default();
    let comp = Comp::new(200, 100, FrameRate::FPS_30, Tick::from_seconds_f64(2.0));
    let cid = p.add_item("Main", Label::Sandstone, None, ItemKind::Comp(comp.clone().into()));
    let inner = Comp::new(100, 100, FrameRate::FPS_30, Tick::from_seconds_f64(2.0));
    let iid = p.add_item("Inner", Label::Sandstone, None, ItemKind::Comp(inner.clone().into()));
    let nested = solid(&mut p, &inner, [1.0, 0.5, 0.0], 60, 60);
    let mut drawn = vec![(iid, nested.id)];
    p.comp_mut(iid).unwrap().layers.push(nested);
    let pre = build::layer(&mut p, &comp, "Inner", LayerSource::Comp { item: iid }, (100, 100), None);
    let mut cube = solid(&mut p, &comp, [0.2, 0.4, 0.8], 80, 40);
    cube.switches.three_d = true;
    let plate = solid(&mut p, &comp, [0.1, 0.1, 0.1], 200, 100);
    for l in [pre, cube, plate] {
        drawn.push((cid, l.id));
        p.comp_mut(cid).unwrap().layers.push(l);
    }
    let times = LayerTimes::default();
    let cache = LayerCache::default();
    let mut r = Renderer::new(&p, &NoFootage, RenderOpts { backend: Backend::Gpu, ..Default::default() });
    r.accel = Some(g);
    r.cache = Some(&cache);
    r.times = Some(&times);
    let img = g.render(&r, cid, Tick::ZERO).expect("the GPU renders the frame");
    assert!(img.get(100, 50)[3] > 0.99);
    let m = times.into_map();
    for key in &drawn {
        assert!(m.get(key).is_some_and(|ms| ms.is_finite() && *ms >= 0.0), "no time for {key:?}: {m:?}");
    }
    assert!(m[&drawn[1]] >= m[&drawn[0]], "the precomp layer includes its nested layer: {m:?}");
}
