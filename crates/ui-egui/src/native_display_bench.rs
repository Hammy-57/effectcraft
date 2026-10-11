#![cfg(all(test, not(target_arch = "wasm32")))]
#[test]
#[ignore = "requires RT_DISPLAY_PROJECT, RT_DISPLAY_REPORT and exclusive native timing window"]
fn native_display_worker_benchmark() {
    use crate::{EffectcraftApp, native_display_tests::request};
    use effectcraft_engine::{
        project::{ItemKind, Project},
        render::{Accelerator, AutoKey, Backend},
    };
    use std::{sync::Arc, time::Instant};
    let project = Project::from_json(&std::fs::read_to_string(std::env::var("RT_DISPLAY_PROJECT").unwrap()).unwrap()).unwrap();
    let cid = project.items.iter().find_map(|(id, item)| matches!(item.kind, ItemKind::Comp(_)).then_some(*id)).unwrap();
    let gpu = effectcraft_gpu::Gpu::try_headless().unwrap();
    gpu.warm_pipelines().unwrap();
    let mut rows = Vec::new();
    for scale in [1.0, 0.5] {
        let mut reference: Option<Arc<egui::ColorImage>> = None;
        for display in [false, true, true, false] {
            let mut session = effectcraft_host::session();
            session.replace_project(project.clone(), None);
            session.open_comp(cid);
            Arc::make_mut(&mut session.project).settings.gpu_acceleration = true;
            session.disk_cache = None;
            let mut app = EffectcraftApp::new(session);
            app.gpu = Some(gpu.clone());
            // A forced-GPU output comparison outside the timed Auto workload.
            let pixels = request(&app, display, Backend::Gpu, 0, scale);
            if let Some(old) = &reference {
                assert_eq!(old.size, pixels.size);
                assert!(
                    old.pixels.iter().zip(&pixels.pixels).all(|(a, b)| a.to_array().iter().zip(b.to_array()).all(|(a, b)| a.abs_diff(b) <= 1)),
                    "display conversion changed output beyond one byte"
                );
            } else {
                reference = Some(pixels);
            }
            app.frames.clear();
            app.session.layer_cache.clear();
            gpu.context().clear_uploads();
            if let Some(pick) = gpu.auto_pick() {
                pick.reset();
            }
            for frame in 0..8 {
                request(&app, display, Backend::Auto, frame, scale);
            }
            let before = gpu.context().transfer_stats();
            let mut samples = Vec::new();
            for frame in 8..68 {
                let start = Instant::now();
                request(&app, display, Backend::Auto, frame, scale);
                samples.push(start.elapsed().as_secs_f64() * 1000.0);
            }
            let after = gpu.context().transfer_stats();
            let stats = gpu.auto_pick().and_then(|pick| pick.stats(AutoKey::new(cid, scale, display)));
            let mut sorted = samples.clone();
            sorted.sort_by(f64::total_cmp);
            let p50 = sorted[29];
            let p95 = sorted[56];
            eprintln!("native display={display} scale={scale} p50={p50:.2} p95={p95:.2}");
            rows.push(serde_json::json!({"display":display,"scale":scale,"p50Ms":p50,"p95Ms":p95,"samplesMs":samples,"readbackBytes":after.readback_bytes-before.readback_bytes,"uploads":after.uploads-before.uploads,"cpuFrames":stats.map(|s|s.cpu_frames),"gpuFrames":stats.map(|s|s.gpu_frames),"cpuMs":stats.and_then(|s|s.cpu_ms),"gpuMs":stats.and_then(|s|s.gpu_ms)}));
        }
    }
    std::fs::write(std::env::var("RT_DISPLAY_REPORT").unwrap(), serde_json::to_vec_pretty(&rows).unwrap()).unwrap();
}
