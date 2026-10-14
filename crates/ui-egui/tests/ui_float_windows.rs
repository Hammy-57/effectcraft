//! Undocked panel groups where egui can't open OS windows (the headless harness, like the web
//! and the snapshot example): they float inside the main window exactly as before, and the state
//! that gives them OS windows on the desktop (`Floating::id`, `Floating::screen`) persists.

use effectcraft_engine::Session;
use effectcraft_ui_egui::EffectcraftApp;
use effectcraft_ui_egui::dock::PanelKind;
use egui_kittest::Harness;
use serde_json::json;

fn harness() -> Harness<'static, EffectcraftApp> {
    let mut s = Session::default();
    // Workspaces save to a settings store; give the session an in-memory one.
    s.config = Some(std::sync::Arc::new(effectcraft_engine::config::MemoryConfig::default()));
    s.execute("comp.new", json!({"name": "A", "width": 320, "height": 180, "duration": 4})).unwrap();
    let mut h = Harness::builder().with_size(egui::vec2(1600.0, 1000.0)).build_eframe(|_| EffectcraftApp::new(s));
    h.run_steps(3);
    h
}

fn invoke(h: &mut Harness<'_, EffectcraftApp>, id: &str, params: serde_json::Value) -> Result<serde_json::Value, String> {
    let ctx = h.ctx.clone();
    effectcraft_ui_egui::menus::invoke(h.state_mut(), &ctx, id, params)
}

fn element(h: &Harness<'_, EffectcraftApp>, id: &str) -> Option<effectcraft_ui_egui::automation::Element> {
    h.state().auto.previous.iter().chain(h.state().auto.elements.iter()).find(|e| e.id == id).cloned()
}

#[test]
fn embedded_floating_groups_stay_in_the_main_window() {
    let mut h = harness();
    assert!(h.ctx.embed_viewports(), "the harness has no OS windows");
    invoke(&mut h, "window.floatPanel", json!({"panel": "Project", "rect": [300, 200, 320, 240]})).unwrap();
    h.run_steps(3);
    let f = h.state().ui.floating.clone();
    assert_eq!(f.len(), 1);
    assert_eq!(f[0].panels, [PanelKind::Project]);
    assert_ne!(f[0].id, 0, "the group has a stable id");
    assert_eq!(f[0].screen, None, "no OS window, no screen rect");
    assert_eq!(f[0].rect, [300.0, 200.0, 320.0, 240.0]);
    // Drawn inside the main window, with its move strip and resize grip, as before.
    for id in ["panel.float.0.move", "panel.float.0.resize", "panel.Project", "panel.tab.Project"] {
        let e = element(&h, id).unwrap_or_else(|| panic!("{id}"));
        assert_eq!(e.window, None, "{id} is in the main window");
    }
    let body = element(&h, "panel.Project").unwrap();
    assert!(body.rect[0] >= 300.0 && body.rect[1] >= 200.0, "{:?}", body.rect);
    // Its panel menu opens in the main window and docks it back.
    invoke(&mut h, "window.panelMenu", json!({"panel": "Project"})).unwrap();
    h.run_steps(2);
    assert!(element(&h, "panelMenu.dockPanel").is_some_and(|e| e.window.is_none()));
    invoke(&mut h, "window.dockPanel", json!({"panel": "Project"})).unwrap();
    h.run_steps(2);
    assert!(h.state().ui.floating.is_empty() && h.state().ui.dock.contains(PanelKind::Project));
}

#[test]
fn floating_window_state_survives_a_relaunch() {
    let mut h = harness();
    invoke(&mut h, "window.floatPanelGroup", json!({"panel": "Timeline"})).unwrap();
    h.run_steps(2);
    let id = h.state().ui.floating[0].id;
    // As the desktop records it: an OS window on a monitor left of the main window.
    h.state_mut().ui.floating[0].screen = Some([-1700.0, 60.0, 1200.0, 380.0]);
    h.run_steps(2);
    let saved = serde_json::to_value(&h.state().ui).unwrap();
    let back: effectcraft_ui_egui::state::UiState = serde_json::from_value(saved.clone()).unwrap();
    assert_eq!(back.floating, h.state().ui.floating);
    assert_eq!((back.floating[0].id, back.floating[0].screen), (id, Some([-1700.0, 60.0, 1200.0, 380.0])));
    // UI state saved before groups had OS windows still loads.
    let mut old = saved;
    for f in old["floating"].as_array_mut().unwrap() {
        let f = f.as_object_mut().unwrap();
        f.remove("id");
        f.remove("screen");
    }
    let old: effectcraft_ui_egui::state::UiState = serde_json::from_value(old).unwrap();
    assert_eq!(old.floating[0].panels, h.state().ui.floating[0].panels);
    assert_eq!((old.floating[0].id, old.floating[0].screen), (0, None));
    // A saved workspace keeps them too.
    invoke(&mut h, "window.saveWorkspace", json!({})).unwrap();
    let ws = h.state().ui.workspace.clone();
    assert_eq!(h.state().ui.saved_floating.get(&ws).map(|f| f[0].screen), Some(Some([-1700.0, 60.0, 1200.0, 380.0])));
}
