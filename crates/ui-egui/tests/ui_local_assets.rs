//! Cursor images are sticky in egui: leaving the viewer must restore the normal cursor.
use effectcraft_engine::Session;
use effectcraft_ui_egui::{EffectcraftApp, state::Tool};
use egui::{Event, pos2};
use egui_kittest::Harness;
use serde_json::json;

#[test]
fn custom_cursor_is_scoped_to_viewer() {
    let mut session = Session::default();
    session.execute("comp.new", json!({"name": "Cursor check", "width": 100, "height": 100})).unwrap();
    let mut h = Harness::builder().with_size(egui::vec2(1600.0, 1000.0)).build_eframe(|_| EffectcraftApp::new(session));
    h.run_steps(4);
    h.state_mut().ui.tool = Tool::Zoom;
    let rect = h.state().auto.find("viewer.comp").unwrap().rect;
    h.event(Event::PointerMoved(pos2(rect[0] + rect[2] / 2.0, rect[1] + rect[3] / 2.0)));
    h.run_steps(3);
    if std::env::var_os("EFFECTCRAFT_ICON_REFERENCE").is_some() {
        let cursor = h.output().platform_output.cursor_image.as_ref().expect("configured cursor must load");
        assert_eq!(cursor.hotspot, [8, 8]);
        assert_eq!(cursor.rgba.len(), usize::from(cursor.size[0]) * usize::from(cursor.size[1]) * 4);
    }
    h.event(Event::PointerMoved(pos2(10.0, 10.0)));
    h.run_steps(3);
    assert!(h.output().platform_output.cursor_image.is_none(), "native cursor leaked outside the viewer");
}
