//! Panel Group Settings ▸ Solo Panels in Stack and Small Tabs (egui_kittest), as in After
//! Effects: Solo keeps one panel of a stacked panel group expanded (expanding one collapses the
//! others); Small Tabs gives a group a shorter tab strip. Both are `window.*` commands, check
//! items of the panel menu, and stay with the layout when a workspace is saved and restored.

use effectcraft_engine::Session;
use effectcraft_ui_egui::EffectcraftApp;
use effectcraft_ui_egui::dock::PanelKind;
use effectcraft_ui_egui::panels::panel_menu::{self, Entry};
use egui::{Event, Pos2, pos2};
use egui_kittest::Harness;
use serde_json::{Value, json};

/// The Default workspace (its right column is a stack: Preview and Properties expanded).
fn harness() -> Harness<'static, EffectcraftApp> {
    let mut s = Session::default();
    s.execute("comp.new", json!({"name": "A", "width": 320, "height": 180, "duration": 4})).unwrap();
    let mut h = Harness::builder().with_size(egui::vec2(1600.0, 1000.0)).build_eframe(|_| EffectcraftApp::new(s));
    h.run_steps(3);
    h
}

fn find(h: &Harness<'_, EffectcraftApp>, id: &str) -> Option<egui::Rect> {
    let e = h.state().auto.elements.iter().find(|e| e.id == id)?;
    Some(egui::Rect::from_min_size(pos2(e.rect[0], e.rect[1]), egui::vec2(e.rect[2], e.rect[3])))
}

fn rect(h: &Harness<'_, EffectcraftApp>, id: &str) -> egui::Rect {
    find(h, id).unwrap_or_else(|| panic!("no element {id}"))
}

fn click(h: &mut Harness<'_, EffectcraftApp>, p: Pos2, button: egui::PointerButton) {
    h.event(Event::PointerMoved(p));
    h.step();
    h.input_mut().events.push(Event::PointerButton { pos: p, button, pressed: true, modifiers: Default::default() });
    h.input_mut().events.push(Event::PointerButton { pos: p, button, pressed: false, modifiers: Default::default() });
    h.step();
    h.run_steps(2);
}

/// Right-click `tab`, open Panel Group Settings ▸ and click `key`.
fn choose(h: &mut Harness<'_, EffectcraftApp>, tab: &str, key: &str) {
    click(h, rect(h, tab).center(), egui::PointerButton::Secondary);
    let sub = rect(h, "panelMenu.groupSettings");
    h.event(Event::PointerMoved(sub.center()));
    h.run_steps(3);
    let item = rect(h, &format!("panelMenu.{key}"));
    h.event(Event::PointerMoved(pos2(item.min.x + 8.0, sub.center().y)));
    h.step();
    click(h, rect(h, &format!("panelMenu.{key}")).center(), egui::PointerButton::Primary);
}

fn invoke(h: &mut Harness<'_, EffectcraftApp>, id: &str, params: Value) -> Result<Value, String> {
    let ctx = h.ctx.clone();
    let r = effectcraft_ui_egui::menus::invoke(h.state_mut(), &ctx, id, params);
    h.run_steps(2);
    r
}

/// (enabled, checked) of the panel menu item `key` of `panel`.
fn item(h: &Harness<'_, EffectcraftApp>, panel: PanelKind, key: &str) -> (bool, bool) {
    fn rec(v: &[Entry], key: &str) -> Option<(bool, bool)> {
        v.iter().find_map(|e| match e {
            Entry::Item { key: k, enabled, checked, command, .. } if k == key => Some((*enabled && !command.is_empty(), *checked)),
            Entry::Sub { children, .. } => rec(children, key),
            _ => None,
        })
    }
    rec(&panel_menu::entries(h.state(), panel, None), key).unwrap_or_else(|| panic!("no item {key}"))
}

fn expanded(h: &Harness<'_, EffectcraftApp>) -> Vec<PanelKind> {
    [PanelKind::Preview, PanelKind::Properties, PanelKind::Align, PanelKind::Audio, PanelKind::EffectsPresets]
        .into_iter()
        .filter(|p| h.state().ui.dock.is_visible(*p))
        .collect()
}

#[test]
fn solo_panels_keeps_one_stacked_panel_expanded() {
    let mut h = harness();
    // Only in a stacked panel group.
    assert_eq!(item(&h, PanelKind::Project, "soloPanels"), (false, false));
    assert!(invoke(&mut h, "window.soloPanels", json!({"panel": "Project"})).is_err());
    assert_eq!(item(&h, PanelKind::Properties, "soloPanels"), (true, false));
    assert_eq!(expanded(&h), [PanelKind::Preview, PanelKind::Properties]);
    // From the panel menu of Properties: it stays expanded, the others collapse.
    choose(&mut h, "panel.tab.Properties", "soloPanels");
    assert_eq!(h.state().ui.dock.solo(PanelKind::Preview), Some(true));
    assert_eq!(item(&h, PanelKind::Properties, "soloPanels"), (true, true));
    assert_eq!(expanded(&h), [PanelKind::Properties]);
    // Clicking another header expands it and collapses the open one.
    let r = rect(&h, "panel.tab.Align");
    click(&mut h, r.center(), egui::PointerButton::Primary);
    assert_eq!(expanded(&h), [PanelKind::Align]);
    assert!(find(&h, "panel.Align").is_some() && find(&h, "panel.Properties").is_none());
    // Window ▸ <panel> shows the panel the same way.
    invoke(&mut h, "window.panel", json!({"panel": "Audio"})).unwrap();
    assert_eq!(expanded(&h), [PanelKind::Audio]);
    // Off again (as a command with an explicit state): several panels may be expanded.
    assert_eq!(invoke(&mut h, "window.soloPanels", json!({"panel": "Audio", "solo": false})).unwrap(), json!({"solo": false}));
    let r = rect(&h, "panel.tab.Preview");
    click(&mut h, r.center(), egui::PointerButton::Primary);
    assert_eq!(expanded(&h), [PanelKind::Preview, PanelKind::Audio]);
    assert_eq!(item(&h, PanelKind::Audio, "soloPanels"), (true, false));
    // Floating groups have no stack.
    invoke(&mut h, "window.floatPanel", json!({"panel": "Align"})).unwrap();
    assert_eq!(item(&h, PanelKind::Align, "soloPanels"), (false, false));
}

#[test]
fn small_tabs_shorten_the_tab_strip() {
    let mut h = harness();
    let strip = rect(&h, "panel.strip.Project");
    let body = rect(&h, "panel.Project");
    let header = rect(&h, "panel.tab.Preview");
    assert_eq!(item(&h, PanelKind::Project, "smallTabs"), (true, false));
    choose(&mut h, "panel.tab.Project", "smallTabs");
    assert_eq!(h.state().ui.dock.small_tabs(PanelKind::EffectControls), Some(true));
    assert_eq!(item(&h, PanelKind::Project, "smallTabs"), (true, true));
    let small = rect(&h, "panel.strip.Project");
    assert!(small.height() < strip.height() - 4.0, "{} → {}", strip.height(), small.height());
    assert!((rect(&h, "panel.tab.Project").height() - small.height()).abs() < 0.5, "the tabs fit the strip");
    // The panel gets the room.
    let taller = rect(&h, "panel.Project");
    assert!((taller.min.y - (body.min.y - (strip.height() - small.height()))).abs() < 0.5, "{body:?} → {taller:?}");
    // Other groups keep their size; the panel menu still opens from a small tab.
    assert!((rect(&h, "panel.strip.Composition").height() - strip.height()).abs() < 0.5);
    assert!(find(&h, "panel.menu.Project").is_some());
    // A stack's headers get small too (the command toggles).
    assert_eq!(invoke(&mut h, "window.smallTabs", json!({"panel": "Properties"})).unwrap(), json!({"small": true}));
    assert!(rect(&h, "panel.tab.Preview").height() < header.height() - 4.0);
    assert_eq!(invoke(&mut h, "window.smallTabs", json!({"panel": "Properties"})).unwrap(), json!({"small": false}));
    assert!((rect(&h, "panel.tab.Preview").height() - header.height()).abs() < 0.5);
    // Explicit off.
    invoke(&mut h, "window.smallTabs", json!({"panel": "Project", "small": false})).unwrap();
    assert!((rect(&h, "panel.strip.Project").height() - strip.height()).abs() < 0.5);
    assert!(invoke(&mut h, "window.smallTabs", json!({"panel": "Tracker"})).is_err(), "not docked");
}

#[test]
fn solo_and_small_tabs_persist_with_workspaces() {
    let mut h = harness();
    invoke(&mut h, "window.smallTabs", json!({"panel": "Project", "small": true})).unwrap();
    invoke(&mut h, "window.soloPanels", json!({"panel": "Preview", "solo": true})).unwrap();
    assert_eq!(expanded(&h), [PanelKind::Preview]);
    invoke(&mut h, "window.saveWorkspaceAs", json!({"name": "Compact"})).unwrap();
    // Another workspace has neither; the saved one brings both back.
    invoke(&mut h, "window.workspace", json!({"name": "Animation"})).unwrap();
    assert_ne!(h.state().ui.dock.small_tabs(PanelKind::Project), Some(true));
    invoke(&mut h, "window.workspace", json!({"name": "Compact"})).unwrap();
    assert_eq!(h.state().ui.dock.small_tabs(PanelKind::Project), Some(true));
    assert_eq!(h.state().ui.dock.solo(PanelKind::Properties), Some(true));
    assert_eq!(expanded(&h), [PanelKind::Preview]);
    // The UI state (what the app keeps between sessions) round-trips them.
    let json = serde_json::to_value(&h.state().ui).unwrap();
    let back: effectcraft_ui_egui::state::UiState = serde_json::from_value(json).unwrap();
    assert_eq!(back.dock, h.state().ui.dock);
    assert_eq!(back.saved_workspaces.get("Compact").and_then(|d| d.solo(PanelKind::Align)), Some(true));
}
