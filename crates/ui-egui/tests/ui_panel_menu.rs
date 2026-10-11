//! Panel menus (egui_kittest), as in After Effects: right-clicking a panel tab opens its panel
//! menu (the ≡ menu's items), whose Close Panel, Undock Panel, Close Other Panels in Group and
//! Panel Group Settings ▸ items act on the panel and its group; each is a `window.*` command.

use effectcraft_engine::Session;
use effectcraft_engine::project::ItemId;
use effectcraft_ui_egui::EffectcraftApp;
use effectcraft_ui_egui::dock::{DockNode, PanelKind};
use egui::{Event, Pos2, pos2};
use egui_kittest::Harness;
use serde_json::json;

/// Comps "A" and "B", both open (B shown), in the Default workspace.
fn harness() -> (Harness<'static, EffectcraftApp>, u64, u64) {
    let mut s = Session::default();
    let a = s.execute("comp.new", json!({"name": "A", "width": 320, "height": 180, "duration": 4})).unwrap()["comp"].as_u64().unwrap();
    let b = s.execute("comp.new", json!({"name": "B", "width": 320, "height": 180, "duration": 4})).unwrap()["comp"].as_u64().unwrap();
    s.state.open_comps = vec![ItemId(a), ItemId(b)];
    s.state.active_comp = Some(ItemId(b));
    let mut h = Harness::builder().with_size(egui::vec2(1600.0, 1000.0)).build_eframe(|_| EffectcraftApp::new(s));
    h.run_steps(3);
    (h, a, b)
}

fn find(h: &Harness<'_, EffectcraftApp>, id: &str) -> Option<egui::Rect> {
    let e = h.state().auto.previous.iter().chain(h.state().auto.elements.iter()).find(|e| e.id == id)?;
    Some(egui::Rect::from_min_size(pos2(e.rect[0], e.rect[1]), egui::vec2(e.rect[2], e.rect[3])))
}

fn rect(h: &Harness<'_, EffectcraftApp>, id: &str) -> egui::Rect {
    find(h, id).unwrap_or_else(|| {
        let have: Vec<&String> = h.state().auto.elements.iter().map(|e| &e.id).filter(|i| i.starts_with("panel")).collect();
        panic!("no element {id}; have {have:?}")
    })
}

fn click(h: &mut Harness<'_, EffectcraftApp>, p: Pos2, button: egui::PointerButton) {
    h.event(Event::PointerMoved(p));
    h.step();
    h.input_mut().events.push(Event::PointerButton { pos: p, button, pressed: true, modifiers: Default::default() });
    h.input_mut().events.push(Event::PointerButton { pos: p, button, pressed: false, modifiers: Default::default() });
    h.step();
    h.run_steps(2);
}

/// Right-click element `id` (a tab) and wait for its menu.
fn menu(h: &mut Harness<'_, EffectcraftApp>, id: &str) {
    let r = rect(h, id);
    click(h, r.center(), egui::PointerButton::Secondary);
    assert!(find(h, "panelMenu.closePanel").is_some(), "the panel menu is open");
}

/// Click a menu item (hovering `sub` first to open that submenu).
fn choose(h: &mut Harness<'_, EffectcraftApp>, sub: Option<&str>, key: &str) {
    if let Some(s) = sub {
        let r = rect(h, &format!("panelMenu.{s}"));
        h.event(Event::PointerMoved(r.center()));
        h.run_steps(3);
        // Along the row into the submenu, as a hand would.
        let item = rect(h, &format!("panelMenu.{key}"));
        h.event(Event::PointerMoved(pos2(item.min.x + 8.0, r.center().y)));
        h.step();
    }
    let r = rect(h, &format!("panelMenu.{key}"));
    click(h, r.center(), egui::PointerButton::Primary);
}

fn group_of(n: &DockNode, p: PanelKind) -> Option<Vec<PanelKind>> {
    match n {
        DockNode::Split { a, b, .. } => group_of(a, p).or_else(|| group_of(b, p)),
        DockNode::Tabs { panels, .. } => panels.contains(&p).then(|| panels.clone()),
        DockNode::Stack { entries, .. } => entries.iter().any(|e| e.contains(p)).then(|| entries.iter().flat_map(|e| e.panels.iter().copied()).collect()),
    }
}

fn menu_open(h: &Harness<'_, EffectcraftApp>) -> bool {
    h.state().auto.elements.iter().any(|e| e.id == "panelMenu.closePanel")
}

#[test]
fn right_click_close_panel_and_close_others() {
    let (mut h, _, _) = harness();
    // Right-clicking Effect Controls' tab brings it forward and opens its menu.
    menu(&mut h, "panel.tab.EffectControls");
    assert!(h.state().ui.dock.is_visible(PanelKind::EffectControls));
    choose(&mut h, None, "closePanel");
    assert!(!h.state().ui.dock.contains(PanelKind::EffectControls));
    assert!(!menu_open(&h), "the menu closes");
    // Close Other Panels in Group on the Composition tab leaves it alone in its group.
    assert_eq!(group_of(&h.state().ui.dock, PanelKind::Composition), Some(vec![PanelKind::Composition, PanelKind::Layer]));
    menu(&mut h, "panel.tab.Composition");
    choose(&mut h, None, "closeOtherPanels");
    assert_eq!(group_of(&h.state().ui.dock, PanelKind::Composition), Some(vec![PanelKind::Composition]));
    assert!(h.state().ui.dock.contains(PanelKind::Timeline));
    // The empty part of a tab strip opens the shown panel's menu; Escape closes it.
    let strip = rect(&h, "panel.strip.Project");
    click(&mut h, pos2(strip.max.x - 30.0, strip.center().y), egui::PointerButton::Secondary);
    assert!(menu_open(&h));
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    assert!(!menu_open(&h));
}

#[test]
fn undock_redock_and_undock_group() {
    let (mut h, _, _) = harness();
    menu(&mut h, "panel.tab.Project");
    choose(&mut h, None, "floatPanel");
    let floating = |h: &Harness<'_, EffectcraftApp>| h.state().ui.floating.iter().map(|f| f.panels.clone()).collect::<Vec<_>>();
    assert_eq!(floating(&h), vec![vec![PanelKind::Project]]);
    assert!(!h.state().ui.dock.contains(PanelKind::Project));
    // A floating panel's menu docks it back with its old neighbours.
    menu(&mut h, "panel.tab.Project");
    choose(&mut h, None, "dockPanel");
    assert!(floating(&h).is_empty());
    assert_eq!(group_of(&h.state().ui.dock, PanelKind::Project), Some(vec![PanelKind::Project, PanelKind::EffectControls]));
    // Panel Group Settings ▸ Undock Panel Group floats the whole group, tabs kept.
    menu(&mut h, "panel.tab.Timeline");
    choose(&mut h, Some("groupSettings"), "floatPanelGroup");
    assert_eq!(floating(&h), vec![vec![PanelKind::Timeline, PanelKind::RenderQueue]]);
    assert!(!h.state().ui.dock.contains(PanelKind::Timeline));
}

#[test]
fn maximize_and_stack_panel_group() {
    let (mut h, _, _) = harness();
    menu(&mut h, "panel.tab.Composition");
    choose(&mut h, Some("groupSettings"), "maximizePanel");
    assert_eq!(h.state().ui.maximized, Some(PanelKind::Composition));
    // The same item (now checked) restores it, as ` does.
    menu(&mut h, "panel.tab.Composition");
    choose(&mut h, Some("groupSettings"), "maximizePanel");
    assert_eq!(h.state().ui.maximized, None);
    // Stacked Panel Group turns the tab group into a stack and back.
    let ctx = h.ctx.clone();
    let st = |h: &mut Harness<'_, EffectcraftApp>, p: serde_json::Value| {
        effectcraft_ui_egui::menus::invoke(h.state_mut(), &ctx, "window.stackPanelGroup", p).unwrap()
    };
    assert_eq!(st(&mut h, json!({"panel": "Project"})), json!({"stacked": true}));
    assert!(h.state().ui.dock.is_stacked(PanelKind::EffectControls));
    assert_eq!(st(&mut h, json!({"panel": "Project"})), json!({"stacked": false}));
    assert_eq!(group_of(&h.state().ui.dock, PanelKind::Project), Some(vec![PanelKind::Project, PanelKind::EffectControls]));
    // Close Panel Group closes every panel of the group.
    menu(&mut h, "panel.tab.Project");
    choose(&mut h, Some("groupSettings"), "closePanelGroup");
    assert!(!h.state().ui.dock.contains(PanelKind::Project) && !h.state().ui.dock.contains(PanelKind::EffectControls));
}

#[test]
fn timeline_comp_tabs_close_their_comp() {
    let (mut h, a, b) = harness();
    let open = |h: &Harness<'_, EffectcraftApp>| h.state().session.state.open_comps.iter().map(|c| c.0).collect::<Vec<_>>();
    // Close Other Panels in Group on B's tab closes A's Timeline tab and the Render Queue.
    menu(&mut h, &format!("panel.tab.Timeline.{b}"));
    choose(&mut h, None, "closeOtherPanels");
    assert_eq!(open(&h), vec![b]);
    assert_eq!(group_of(&h.state().ui.dock, PanelKind::Timeline), Some(vec![PanelKind::Timeline]));
    // Close Panel on a comp tab closes that comp's Timeline; the panel stays.
    h.state_mut().session.execute("comp.open", json!({"comp": a})).unwrap();
    h.run_steps(3);
    menu(&mut h, &format!("panel.tab.Timeline.{a}"));
    choose(&mut h, None, "closePanel");
    assert_eq!(open(&h), vec![b]);
    assert!(h.state().ui.dock.contains(PanelKind::Timeline));
    // File ▸ Close (Cmd+W) closes the focused panel unless it shows a comp.
    let ctx = h.ctx.clone();
    h.state_mut().ui.focused = PanelKind::Project;
    effectcraft_ui_egui::menus::invoke(h.state_mut(), &ctx, "file.close", json!({})).unwrap();
    assert!(!h.state().ui.dock.contains(PanelKind::Project));
}
