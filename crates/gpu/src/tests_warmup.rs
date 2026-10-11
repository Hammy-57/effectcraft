use crate::tests::{Pattern, Scene};
use effectcraft_project::BitDepth;
use effectcraft_render::{Accelerator, Backend, NoFootage, RenderOpts, Renderer};
use effectcraft_time::Tick;

#[test]
fn warmup_all_static_effects_adds_no_pipelines() {
    crate::tests::hold_gpu_lock();
    let gpu = crate::Gpu::try_headless();
    assert!(gpu.is_ok() || std::env::var("EFFECTCRAFT_REQUIRE_GPU").as_deref() != Ok("1"));
    let Ok(gpu) = gpu else { return };
    let cold = gpu.static_pipeline_count();
    // Warmup must not change the existing Advanced 3D output. Compare the same
    // GPU before/after warmup, avoiding unrelated CPU/GPU silhouette differences.
    let (project, cid) = crate::tests_adv3d::scene_project();
    let render_advanced = || {
        let mut renderer = Renderer::new(&project, &NoFootage, RenderOpts { backend: Backend::Gpu, ..Default::default() });
        renderer.accel = Some(&gpu);
        gpu.render(&renderer, cid, Tick::ZERO).expect("Advanced 3D frame")
    };
    let advanced_before = render_advanced();
    assert!(advanced_before.data.iter().any(|p| p[3] > 0.0));
    std::thread::scope(|s| {
        let a = s.spawn(|| gpu.warm_pipelines());
        let b = s.spawn(|| gpu.warm_pipelines());
        a.join().unwrap().unwrap();
        b.join().unwrap().unwrap();
    });
    let warmed = gpu.static_pipeline_count();
    assert!(warmed > cold);
    let advanced_after = render_advanced();
    assert_eq!(advanced_after.data, advanced_before.data, "warmup changed Advanced 3D pixels");
    assert_eq!(gpu.static_pipeline_count(), warmed, "Advanced 3D created a pipeline after warmup");
    for id in effectcraft_effects::GPU_EFFECTS {
        let mut scene = Scene::new(BitDepth::Bpc8);
        let mut layer = scene.footage(16, 16);
        scene.effect(&mut layer, id, &[]);
        scene.push(layer);
        let mut renderer = Renderer::new(&scene.p, &Pattern, RenderOpts { backend: Backend::Gpu, ..Default::default() });
        renderer.accel = Some(&gpu);
        assert!(gpu.render(&renderer, scene.cid, Tick::ZERO).is_some(), "{id}");
        assert_eq!(gpu.static_pipeline_count(), warmed, "{id} created a static pipeline after warmup");
    }
    gpu.warm_pipelines().unwrap();
    assert_eq!(gpu.static_pipeline_count(), warmed);
}

#[test]
fn warmup_failure_is_cached_and_cpu_path_remains_available() {
    crate::tests::hold_gpu_lock();
    let gpu = crate::Gpu::try_headless();
    assert!(gpu.is_ok() || std::env::var("EFFECTCRAFT_REQUIRE_GPU").as_deref() != Ok("1"));
    let Ok(gpu) = gpu else { return };
    assert!(gpu.ctx.adv3d.set(Err("synthetic pipeline failure".into())).is_ok());
    let count = gpu.static_pipeline_count();
    for _ in 0..2 {
        assert_eq!(gpu.warm_pipelines(), Err("synthetic pipeline failure".into()));
    }
    assert_eq!(gpu.static_pipeline_count(), count);
    let (mut project, cid) = crate::tests_adv3d::scene_project();
    project.settings.bit_depth = BitDepth::Bpc32;
    let advanced = crate::tests_adv3d::scene(&project, cid);
    assert!(!advanced.indices.is_empty(), "fixture exercises the failed raster pipeline");
    assert!(gpu.raster_3d(&advanced).is_none(), "failed optional pipeline must decline GPU rasterization");
    let cpu = Renderer::new(&project, &NoFootage, RenderOpts { backend: Backend::Cpu, ..Default::default() }).comp_frame(cid, Tick::ZERO);
    let mut renderer = Renderer::new(&project, &NoFootage, RenderOpts { backend: Backend::Gpu, ..Default::default() });
    renderer.accel = Some(&gpu);
    let fallback = renderer.comp_frame(cid, Tick::ZERO);
    assert!(cpu.data.iter().any(|p| p[3] > 0.0));
    assert_eq!((fallback.width, fallback.height), (cpu.width, cpu.height));
    assert!(fallback.data.iter().flatten().all(|v| v.is_finite()), "fallback pixels must be finite");
    let diff = crate::tests::diff(&cpu, &fallback, 1e-3);
    assert_eq!(diff.over, 0, "failed Advanced 3D pipeline fallback differs: max {}", diff.max);
    assert_eq!(gpu.static_pipeline_count(), count, "failed pipeline must not retry compilation");
}

#[test]
fn warmup_particle_failure_falls_back_with_accelerator_attached() {
    crate::tests::hold_gpu_lock();
    let gpu = crate::Gpu::try_headless();
    assert!(gpu.is_ok() || std::env::var("EFFECTCRAFT_REQUIRE_GPU").as_deref() != Ok("1"));
    let Ok(gpu) = gpu else { return };
    assert!(gpu.ctx.particles.set(Err("synthetic particle pipeline failure".into())).is_ok());
    for _ in 0..2 {
        assert_eq!(gpu.warm_pipelines(), Err("synthetic particle pipeline failure".into()));
    }
    let count = gpu.static_pipeline_count();
    let mut scene = Scene::new(BitDepth::Bpc32);
    let mut layer = scene.solid([0.0, 0.0, 0.0], 70, 44);
    scene.effect(
        &mut layer,
        "ec.sim.ccparticleworld",
        &[("birthRate", crate::tests::n(2.0)), ("particle/particleType", effectcraft_keyframe::Value::Enum(1)), ("particle/birthSize", crate::tests::n(0.3))],
    );
    scene.push(layer);
    let at = Tick::from_seconds_f64(0.5);
    let cpu = Renderer::new(&scene.p, &Pattern, RenderOpts { backend: Backend::Cpu, ..Default::default() }).comp_frame(scene.cid, at);
    assert!(cpu.data.iter().any(|p| p[0] > 0.0 || p[1] > 0.0 || p[2] > 0.0), "fixture draws particles over black");
    let mut renderer = Renderer::new(&scene.p, &Pattern, RenderOpts { backend: Backend::Gpu, ..Default::default() });
    renderer.accel = Some(&gpu);
    let fallback = renderer.comp_frame(scene.cid, at);
    assert_eq!((fallback.width, fallback.height), (cpu.width, cpu.height));
    assert!(fallback.data.iter().flatten().all(|v| v.is_finite()), "fallback pixels must be finite");
    let diff = crate::tests::diff(&cpu, &fallback, 1e-3);
    assert_eq!(diff.over, 0, "failed particle pipeline fallback differs: max {}", diff.max);
    assert_eq!(gpu.static_pipeline_count(), count, "failed particle pipeline must not retry compilation");
}
