//! Opt-in native RT5 benchmark: real viewer workers and the RT0 generated scene.
#![cfg(all(test, not(target_arch = "wasm32")))]

#[test]
#[ignore = "requires RT5_PROJECT from RT0 and an exclusive native GPU benchmark window"]
fn adaptive_playback_frame_availability() {
    use crate::{EffectcraftApp, frames::FrameImage, state::Resolution};
    use effectcraft_engine::commands::viewer_cmds::FastPreviews;
    use effectcraft_engine::project::{ItemKind, Project};
    use std::sync::Arc;
    use std::time::{Duration, Instant};
    let path = std::env::var("RT5_PROJECT").unwrap();
    let project = Project::from_json(&std::fs::read_to_string(path).unwrap()).unwrap();
    let cid = project.items.iter().find_map(|(id, item)| matches!(item.kind, ItemKind::Comp(_)).then_some(*id)).unwrap();
    let gpu = effectcraft_gpu::Gpu::try_headless().unwrap();
    gpu.warm_pipelines().unwrap();
    let mut runs = Vec::new();
    for adaptive in [false, true, true, false] {
        gpu.context().clear_uploads();
        let mut session = effectcraft_host::session();
        session.replace_project(project.clone(), None);
        session.open_comp(cid);
        Arc::make_mut(&mut session.project).settings.gpu_acceleration = true;
        session.disk_cache = None;
        let mut app = EffectcraftApp::new(session);
        app.gpu = Some(gpu.clone());
        app.ui.viewer.res = Resolution::Auto;
        app.session.state.viewer.fast_previews = if adaptive { FastPreviews::AdaptiveResolution } else { FastPreviews::Off };
        app.playback.playing = true;
        app.playback.fps = 30.0;
        let mut samples = Vec::new();
        let mut scales = Vec::new();
        let mut first_reduction = None;
        for frame in 0..76 {
            let scale = app.viewer_scale(1.0, 1.0);
            if scale < 1.0 && first_reduction.is_none() { first_reduction = Some(frame); }
            let key = app.frame_key(cid, frame, scale);
            let start = Instant::now();
            app.request_frame_urgent(cid, frame, scale);
            while !app.frames.is_cached(&key) && start.elapsed() < Duration::from_secs(30) {
                std::thread::sleep(Duration::from_millis(1));
            }
            let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;
            let image = app.frames.get(&key).expect("viewer frame must finish");
            let comp = app.session.project.comp(cid).unwrap();
            assert_eq!(image.size(), [(comp.width as f64 * scale).round() as usize, (comp.height as f64 * scale).round() as usize]);
            if let FrameImage::Cpu(image) = image { assert!(!image.pixels.is_empty()); }
            samples.push(elapsed_ms);
            scales.push(scale);
        }
        let mut steady = samples.iter().skip(16).copied().collect::<Vec<_>>();
        steady.sort_by(f64::total_cmp);
        let p50 = steady.get(29).copied().unwrap();
        let p95 = steady.get(56).copied().unwrap();
        assert_eq!(app.ui.viewer.res, Resolution::Auto);
        app.playback.playing = false;
        assert_eq!(app.viewer_scale(1.0, 1.0), 1.0);
        eprintln!("RT5 adaptive={adaptive} first_reduction={first_reduction:?} p50={p50:.2} p95={p95:.2}");
        runs.push(serde_json::json!({"adaptive": adaptive, "firstReductionFrame": first_reduction,
            "p50Ms": p50, "p95Ms": p95, "availabilityMs": samples, "scales": scales}));
    }
    let output = std::env::var("RT5_REPORT").unwrap();
    std::fs::write(output, serde_json::to_vec_pretty(&runs).unwrap()).unwrap();
}
