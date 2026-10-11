//! Opt-in real native worker deadline benchmark with synthetic timeline motion.
#![cfg(all(test, not(target_arch = "wasm32")))]
#[test]
#[ignore = "requires RT5_PROJECT and an exclusive native timing window"]
fn progressive_scrub_deadlines() {
    use crate::{EffectcraftApp, state::Resolution};
    use effectcraft_engine::{
        commands::viewer_cmds::FastPreviews,
        project::{ItemKind, Project},
        render::Accelerator,
    };
    use std::{
        sync::Arc,
        time::{Duration, Instant},
    };
    let project = Project::from_json(&std::fs::read_to_string(std::env::var("RT5_PROJECT").unwrap()).unwrap()).unwrap();
    let cid = project.items.iter().find_map(|(id, item)| matches!(item.kind, ItemKind::Comp(_)).then_some(*id)).unwrap();
    let gpu = effectcraft_gpu::Gpu::try_headless().unwrap();
    gpu.warm_pipelines().unwrap();
    let mut runs = Vec::new();
    for adaptive in [false, true, true, false] {
        gpu.context().clear_uploads();
        if let Some(pick) = gpu.auto_pick() {
            pick.reset();
        }
        let mut session = effectcraft_host::session();
        session.replace_project(project.clone(), None);
        session.open_comp(cid);
        Arc::make_mut(&mut session.project).settings.gpu_acceleration = true;
        session.disk_cache = None;
        let mut app = EffectcraftApp::new(session);
        app.gpu = Some(gpu.clone());
        app.ui.viewer.res = Resolution::Auto;
        app.session.state.viewer.fast_previews = if adaptive { FastPreviews::AdaptiveResolution } else { FastPreviews::Off };
        let coarse = 0.25f64.max(app.session.prefs.adaptive_limit()).min(1.0);
        // Warm AutoPick at both scales equally, outside the measured cursor range.
        for scale in [1.0, coarse] {
            for frame in 128..136 {
                let key = app.frame_key(cid, frame, scale);
                app.request_frame_urgent(cid, frame, scale);
                let start = Instant::now();
                while !app.frames.is_cached(&key) && start.elapsed() < Duration::from_secs(30) {
                    std::thread::sleep(Duration::from_millis(1));
                }
                assert!(app.frames.is_cached(&key));
            }
        }
        let rate = app.session.active_comp().unwrap().frame_rate;
        let start = Instant::now();
        let mut on_time = 0;
        let mut latencies = Vec::new();
        let mut scales = Vec::new();
        let mut last_key = None;
        for event in 0..60 {
            // Forward on even frames, then reverse on odd frames: no measured frame repeats.
            let frame = if event < 30 { 10 + event * 2 } else { 129 - event * 2 };
            let now = start.elapsed().as_secs_f64() + 1.0;
            app.session.set_time(rate.tick_of(frame));
            app.record_scrub(now);
            let scale = app.viewer_scale(1.0, 1.0);
            let key = app.frame_key(cid, frame, scale);
            let requested = Instant::now();
            app.request_frame_urgent(cid, frame, scale);
            let deadline = Duration::from_millis((event as u64 + 1) * 33);
            let mut shown = false;
            while start.elapsed() < deadline {
                if !shown && app.frames.is_cached(&key) {
                    shown = true;
                    on_time += 1;
                    latencies.push(requested.elapsed().as_secs_f64() * 1000.0);
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            scales.push(scale);
            last_key = Some(key);
        }
        let key = last_key.unwrap();
        let settle = Instant::now();
        while !app.frames.is_cached(&key) && settle.elapsed() < Duration::from_secs(30) {
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(app.frames.is_cached(&key));
        let final_coarse_ms = settle.elapsed().as_secs_f64() * 1000.0;
        app.scrub_preview.tick(start.elapsed().as_secs_f64() + 1.2, Some(cid.0), app.session.revision);
        assert_eq!(app.viewer_scale(1.0, 1.0), 1.0);
        let full = app.frame_key(cid, key.frame, 1.0);
        let refine = Instant::now();
        app.request_frame_urgent(cid, key.frame, 1.0);
        while !app.frames.is_cached(&full) && refine.elapsed() < Duration::from_secs(30) {
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(app.frames.is_cached(&full));
        let refine_ms = refine.elapsed().as_secs_f64() * 1000.0;
        eprintln!("RT5 scrub adaptive={adaptive} on_time={on_time}/60 final_coarse_ms={final_coarse_ms:.2} refine_ms={refine_ms:.2}");
        runs.push(serde_json::json!({"adaptive":adaptive,"onTimeOf60":on_time,"latenciesMs":latencies,"scales":scales,"finalCoarseMs":final_coarse_ms,"refineMs":refine_ms}));
    }
    std::fs::write(std::env::var("RT5_REPORT").unwrap(), serde_json::to_vec_pretty(&runs).unwrap()).unwrap();
}
