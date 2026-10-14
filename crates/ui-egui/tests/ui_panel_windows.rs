//! Multiple monitors (#486): a panel moves out of the main window into a native window of its
//! own (Move Panel to New Window), which can be placed on another screen, and back.
use effectcraft_engine::Session;
use effectcraft_ui_egui::EffectcraftApp;
use effectcraft_ui_egui::dock::{PanelKind, PanelWindow};
use egui::vec2;
use egui_kittest::Harness;
use serde_json::json;

fn invoke(app: &mut EffectcraftApp, ctx: &egui::Context, params: serde_json::Value) -> Result<serde_json::Value, String> {
    effectcraft_ui_egui::menus::invoke(app, ctx, "window.panelWindow", params)
}

#[test]
fn panel_window_command_moves_a_docked_panel_into_its_own_window_and_back() {
    let mut app = EffectcraftApp::new(Session::default());
    let ctx = egui::Context::default();
    assert!(app.ui.dock.contains(PanelKind::Project));
    assert_eq!(invoke(&mut app, &ctx, json!({"panel": "Project", "window": true})).unwrap(), json!({"window": true}));
    // Undocked first, then given a window of its own.
    assert!(!app.ui.dock.contains(PanelKind::Project));
    assert!(app.ui.floating.iter().any(|group| group.panels == [PanelKind::Project]));
    assert!(app.in_panel_window(PanelKind::Project));
    let rect = app.ui.floating.iter().find(|g| g.panels.contains(&PanelKind::Project)).unwrap().screen.unwrap();
    assert_eq!(rect, PanelWindow::sanitized(rect));
    // Asking again changes nothing; no `window` toggles.
    assert_eq!(invoke(&mut app, &ctx, json!({"panel": "Project", "window": true})).unwrap(), json!({"window": true}));
    assert!(app.ui.panel_windows.is_empty(), "new windows use Floating.screen");
    assert_eq!(invoke(&mut app, &ctx, json!({"panel": "Project"})).unwrap(), json!({"window": false}));
    // Back in the main window, docked rather than opening another native window.
    assert!(!app.in_panel_window(PanelKind::Project));
    assert!(app.ui.panel_windows.is_empty());
    assert!(app.ui.dock.contains(PanelKind::Project));
    // The serialized panel form works too.
    assert_eq!(invoke(&mut app, &ctx, json!({"panel": PanelKind::Project})).unwrap(), json!({"window": true}));
    // Bad requests are errors and change nothing.
    let before = app.ui.panel_windows.clone();
    for params in [json!({}), json!({"panel": "Nope"}), json!({"panel": 7}), json!({"panel": "Project", "window": "yes"}), json!({"panel": {"Viewer": 9}})] {
        assert!(invoke(&mut app, &ctx, params.clone()).is_err(), "{params} must be rejected");
    }
    assert_eq!(app.ui.panel_windows, before);
}

#[test]
fn a_panel_in_its_own_window_leaves_the_main_window() {
    let mut app = EffectcraftApp::new(Session::default());
    app.ui.start_screen = false;
    let mut h = Harness::builder().with_size(vec2(1200.0, 760.0)).build_eframe(move |_| app);
    h.run_steps(3);
    let ctx = h.ctx.clone();
    invoke(h.state_mut(), &ctx, json!({"panel": "Project", "window": true})).unwrap();
    h.run_steps(3);
    // The headless harness embeds ordinary floating groups; desktop uses their native windows.
    assert!(h.state().auto.elements.iter().any(|e| e.id == "panel.Project"));
    assert!(h.state().in_panel_window(PanelKind::Project));
    // Docking it back into the main window closes the window entry.
    let ctx = h.ctx.clone();
    effectcraft_ui_egui::menus::invoke(h.state_mut(), &ctx, "window.dockPanel", json!({"panel": "Project", "anchor": "Timeline"})).unwrap();
    h.run_steps(2);
    assert!(h.state().ui.panel_windows.is_empty());
    assert!(!h.state().in_panel_window(PanelKind::Project));
}

#[test]
fn panel_windows_survive_restart_with_hostile_places_made_safe() {
    use effectcraft_engine::config::MemoryConfig;
    use std::sync::Arc;
    let store = Arc::new(MemoryConfig::default());
    let mut app = EffectcraftApp::new(Session { config: Some(store.clone()), ..Default::default() });
    let ctx = egui::Context::default();
    invoke(&mut app, &ctx, json!({"panel": "Project", "window": true})).unwrap();
    let hostile = [1.0e9, -3.0e12, 1.0e9, 0.5];
    app.ui.floating.iter_mut().find(|g| g.panels.contains(&PanelKind::Project)).unwrap().screen = Some([10.0, 20.0, 300.0, 200.0]);
    app.ui.panel_windows.push(PanelWindow { panel: PanelKind::Project, rect: hostile });
    // A window for a panel that doesn't float is dropped on load.
    app.ui.panel_windows.push(PanelWindow { panel: PanelKind::Timeline, rect: [10.0, 10.0, 300.0, 200.0] });
    eframe::App::on_exit(&mut app);
    let restarted = EffectcraftApp::new(Session { config: Some(store), ..Default::default() });
    assert!(restarted.ui.panel_windows.is_empty());
    assert_eq!(restarted.ui.floating.iter().find(|g| g.panels.contains(&PanelKind::Project)).unwrap().screen, Some(PanelWindow::sanitized(hostile)));
    let [x, y, w, h] = PanelWindow::sanitized(hostile);
    assert!(x <= 100_000.0 && y >= -100_000.0 && (160.0..=16_384.0).contains(&w) && (100.0..=16_384.0).contains(&h));
    assert!(PanelWindow::sanitized([f32::NAN, f32::INFINITY, f32::NAN, f32::NEG_INFINITY]).iter().all(|v| v.is_finite()));
}

#[test]
fn menu_created_window_uses_named_workspace_geometry() {
    use effectcraft_engine::config::MemoryConfig;
    use std::sync::Arc;
    let mut app = EffectcraftApp::new(Session { config: Some(Arc::new(MemoryConfig::default())), ..Default::default() });
    let ctx = egui::Context::default();
    invoke(&mut app, &ctx, json!({"panel":"Project","window":true})).unwrap();
    let screen = [-1400.0, 80.0, 600.0, 420.0];
    app.ui.floating.iter_mut().find(|g| g.panels.contains(&PanelKind::Project)).unwrap().screen = Some(screen);
    effectcraft_ui_egui::menus::invoke(&mut app, &ctx, "window.saveWorkspaceAs", json!({"name":"Side monitor"})).unwrap();
    app.set_workspace("Default");
    assert!(!app.in_panel_window(PanelKind::Project));
    app.set_workspace("Side monitor");
    assert!(app.in_panel_window(PanelKind::Project));
    assert_eq!(app.ui.floating.iter().find(|g| g.panels.contains(&PanelKind::Project)).unwrap().screen, Some(screen));
    assert!(app.ui.panel_windows.is_empty());
}
