//! Admission pressure without precomp retention or new source producers.
use crate::tests::{self, Pattern, Scene, n, set, v3};
use effectcraft_color::Label;
use effectcraft_keyframe::Keyframe;
use effectcraft_project::{BitDepth, Comp, ItemKind};
use effectcraft_render::{Backend, LayerCache, RenderOpts, Renderer, cache::GpuStage};
use effectcraft_time::{FrameRate, Tick};

#[test]
fn animated_admission_preserves_static_ordinary_processed_under_pressure() {
    let Some(g) = tests::gpu() else {
        assert!(!std::env::var("EFFECTCRAFT_REQUIRE_GPU").is_ok_and(|v| v == "1"));
        return;
    };
    g.ctx.clear_uploads();
    let mut scene = Scene::new(BitDepth::Bpc32);
    let mut still = scene.solid([0.72, 0.31, 0.13], 60, 40);
    scene.effect(&mut still, "ec.blur.gaussian", &[("blurriness", n(1.0))]);
    set(&mut still, "transform/position", v3(35.0, 25.0));
    let static_id = scene.push(still);
    let other = Comp::new(60, 40, FrameRate::FPS_30, Tick::from_seconds_f64(2.0));
    let animated_comp = scene.p.add_item("generated", Label::Sandstone, None, ItemKind::Comp(other.clone().into()));
    let mut moving = Scene { p: std::mem::take(&mut scene.p), cid: animated_comp, comp: other };
    let mut layer = moving.solid([0.2, 0.4, 0.7], 60, 40);
    moving.effect(&mut layer, "ec.blur.gaussian", &[("blurriness", n(1.0))]);
    layer.props.prop_mut("effects/#1/blurriness").unwrap().keys = vec![Keyframe::new(Tick::ZERO, n(1.0)), Keyframe::new(Tick::from_seconds_f64(1.0), n(8.0))];
    let animated = moving.push(layer);
    scene.p = moving.p;
    let cache = LayerCache::new(160 << 10);
    let opts = RenderOpts { backend: Backend::Gpu, ..Default::default() };
    let mut r = Renderer::new(&scene.p, &Pattern, opts);
    r.accel = Some(g);
    r.cache = Some(&cache);
    let ctx = r.eval_ctx(scene.cid, Tick::ZERO).unwrap();
    let layer = ctx.comp.layers.iter().find(|l| l.id == static_id).unwrap();
    assert!(!r.resident_content_animated(&ctx, layer));
    let static_key = r.resident_content_key(&ctx, layer).unwrap();
    let first = g.within_memory(|| g.render_display(&r, scene.cid, Tick::ZERO)).unwrap().unwrap();
    let first = g.read_display(&first).unwrap();
    assert!(cache.gpu_owner().unwrap().resident(static_key, GpuStage::Processed).is_some());
    let before = g.ctx.transfer_stats();
    for frame in 1..=12 {
        let at = FrameRate::FPS_30.tick_of(frame);
        let ctx = r.eval_ctx(animated_comp, at).unwrap();
        let layer = ctx.comp.layers.iter().find(|l| l.id == animated).unwrap();
        assert!(r.resident_content_animated(&ctx, layer));
        let key = r.resident_content_key(&ctx, layer).unwrap();
        g.within_memory(|| g.render_display(&r, animated_comp, at)).unwrap().unwrap();
        assert!(cache.gpu_owner().unwrap().resident(key, GpuStage::Processed).is_none());
        assert!(cache.gpu_owner().unwrap().resident(static_key, GpuStage::Processed).is_some());
    }
    assert_eq!(g.ctx.transfer_stats().readbacks, before.readbacks);
    let before = g.ctx.transfer_stats();
    let again = g.within_memory(|| g.render_display(&r, scene.cid, Tick::ZERO)).unwrap().unwrap();
    assert_eq!(g.ctx.transfer_stats().uploads, before.uploads);
    assert_eq!(first, g.read_display(&again).unwrap());
    let at = FrameRate::FPS_30.tick_of(12);
    let actual = g.render(&r, animated_comp, at).unwrap();
    let cpu = Renderer::new(&scene.p, &Pattern, RenderOpts { backend: Backend::Cpu, ..opts }).comp_frame_cpu(animated_comp, at);
    assert_eq!((actual.width, actual.height), (cpu.width, cpu.height));
    for (a, b) in actual.data.iter().flatten().zip(cpu.data.iter().flatten()) {
        assert!((a - b).abs() <= 1e-3);
    }
}

#[test]
fn ordinary_transform_animation_keeps_content_admissible() {
    let mut scene = Scene::new(BitDepth::Bpc32);
    let mut layer = scene.solid([0.2, 0.4, 0.7], 30, 20);
    layer.props.prop_mut("transform/position").unwrap().keys =
        vec![Keyframe::new(Tick::ZERO, v3(20.0, 20.0)), Keyframe::new(Tick::from_seconds_f64(1.0), v3(40.0, 30.0))];
    scene.push(layer);
    let cache = LayerCache::default();
    let mut r = Renderer::new(&scene.p, &Pattern, RenderOpts { backend: Backend::Gpu, ..Default::default() });
    r.cache = Some(&cache);
    let ctx = r.eval_ctx(scene.cid, Tick::ZERO).unwrap();
    let layer = &ctx.comp.layers[0];
    assert!(!r.resident_content_animated(&ctx, layer));
    let later = ctx.at(Tick::from_seconds_f64(0.5));
    assert_eq!(r.resident_content_key(&ctx, layer), r.resident_content_key(&later, layer));
}

#[test]
fn repeated_single_key_and_held_content_admits_after_second_production() {
    let Some(g) = tests::gpu() else {
        assert!(!std::env::var("EFFECTCRAFT_REQUIRE_GPU").is_ok_and(|v| v == "1"));
        return;
    };
    for held in [false, true] {
        g.ctx.clear_uploads();
        let mut scene = Scene::new(BitDepth::Bpc32);
        let mut layer = scene.solid([0.2, 0.4, 0.7], 30, 20);
        scene.effect(&mut layer, "ec.blur.gaussian", &[("blurriness", n(2.0))]);
        let amount = layer.props.prop_mut("effects/#1/blurriness").unwrap();
        amount.keys = vec![Keyframe::new(Tick::ZERO, n(2.0))];
        if held {
            amount.keys[0].out_interp = effectcraft_keyframe::Interp::Hold;
            amount.keys.push(Keyframe::new(Tick::from_seconds_f64(1.0), n(8.0)));
        }
        let id = scene.push(layer);
        let cache = LayerCache::new(1 << 20);
        let mut r = Renderer::new(&scene.p, &Pattern, RenderOpts { backend: Backend::Gpu, ..Default::default() });
        r.cache = Some(&cache);
        r.accel = Some(g);
        let ctx = r.eval_ctx(scene.cid, Tick::ZERO).unwrap();
        let layer = ctx.comp.layers.iter().find(|l| l.id == id).unwrap();
        assert!(r.resident_content_animated(&ctx, layer));
        let key = r.resident_content_key(&ctx, layer).unwrap();
        for (i, seconds) in [0.0, 0.25, 0.5].into_iter().enumerate() {
            let time = Tick::from_seconds_f64(seconds);
            let at = r.eval_ctx(scene.cid, time).unwrap();
            assert_eq!(r.resident_content_key(&at, layer), Some(key));
            let before = g.ctx.transfer_stats();
            g.within_memory(|| g.render_display(&r, scene.cid, time)).unwrap().unwrap();
            assert_eq!(g.ctx.transfer_stats().readbacks, before.readbacks);
            assert_eq!(cache.gpu_owner().unwrap().resident(key, GpuStage::Processed).is_some(), i >= 1);
            if i == 2 {
                assert_eq!(g.ctx.transfer_stats().uploads, before.uploads, "third visit reuses resident result");
            }
        }
        cache.clear();
        assert!(!cache.admit_repeated_gpu_content(key), "purge resets the doorkeeper");
    }
}
