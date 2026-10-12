#![cfg(test)]
use crate::{EffectcraftApp, state::Resolution};
use effectcraft_engine::{Session, commands::viewer_cmds::FastPreviews};
use serde_json::json;

fn app() -> EffectcraftApp {
    let mut app = EffectcraftApp::new(Session::default());
    app.session.execute("comp.new", json!({"width":64,"height":64,"duration":3})).unwrap();
    app.session.set_resolution(app.session.active_comp_id().unwrap(), Resolution::Auto).unwrap();
    app.session.state.viewer.fast_previews = FastPreviews::AdaptiveResolution;
    app
}
fn motion(app: &mut EffectcraftApp) -> effectcraft_engine::project::ItemId {
    let id = app.session.active_comp_id().unwrap();
    let rate = app.session.active_comp().unwrap().frame_rate;
    app.session.set_time(rate.tick_of(1));
    app.record_scrub(1.0);
    app.session.set_time(rate.tick_of(4));
    app.record_scrub(1.05);
    id
}

#[test]
fn scrub_resolution_respects_overrides_and_refines_on_quiet() {
    let mut app = app();
    let id = motion(&mut app);
    let coarse = 0.25f64.max(app.session.prefs.adaptive_limit()).min(1.0);
    assert_eq!(app.viewer_scale(1.0, 1.0), coarse);
    for resolution in [Resolution::Full, Resolution::Half, Resolution::Quarter] {
        app.session.set_resolution(app.session.active_comp_id().unwrap(), resolution).unwrap();
        assert_eq!(app.viewer_scale(1.0, 1.0), resolution.scale(1.0, 1.0));
    }
    app.session.set_resolution(app.session.active_comp_id().unwrap(), Resolution::Auto).unwrap();
    app.session.state.viewer.fast_previews = FastPreviews::Off;
    assert_eq!(app.viewer_scale(1.0, 1.0), 1.0);
    app.session.state.viewer.fast_previews = FastPreviews::AdaptiveResolution;
    app.scrub_preview.tick(1.20, Some(id.0), app.session.revision);
    assert_eq!(app.viewer_scale(1.0, 1.0), 1.0);
    assert_eq!(app.viewer_res(), Resolution::Auto);
}

#[test]
fn late_coarse_frame_can_refine_without_replacing_ready_full_or_other_content() {
    use std::time::{Duration, Instant};
    // A loaded font database must not replace the document's frame-key revision.
    let _ = effectcraft_engine::text::fonts::resolve("Inter", "Regular");
    let mut app = app();
    let id = motion(&mut app);
    let coarse = app.viewer_scale(1.0, 1.0);
    let key = app.frame_key(id, 4, coarse);
    app.request_frame_urgent(id, 4, coarse);
    let wait = |app: &EffectcraftApp, key: &crate::frames::FrameKey| {
        let start = Instant::now();
        while !app.frames.is_cached(key) && start.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(app.frames.is_cached(key));
    };
    wait(&app, &key);
    app.scrub_preview.tick(1.20, Some(id.0), app.session.revision);
    let full = app.frame_key(id, 4, 1.0);
    assert_eq!(app.frame_for_viewer(&full, 1.0, 1.0).unwrap().0, key);
    assert!(app.frame_for_viewer(&crate::frames::FrameKey { content: full.content ^ 1, ..full }, 1.0, 1.0).is_none());
    assert!(app.frame_for_viewer(&crate::frames::FrameKey { frame: 5, ..full }, 1.0, 1.0).is_none());
    app.request_frame_urgent(id, 4, 1.0);
    wait(&app, &full);
    assert_eq!(app.frame_for_viewer(&full, 1.0, 1.0).unwrap().0, full);
}
