//! The Timeline's layer outline columns as in After Effects, with real pointer input
//! (egui_kittest): drag a header divider to resize a column, a header to reorder, right-click ▸
//! Hide This / Columns to hide and show, click the name header for Source Name / Layer Name, drag
//! the splitter between the outline and the time graph, the footer's pane buttons and Toggle
//! Switches / Modes. The layout comes back on the next launch.

use std::sync::Arc;

use effectcraft_engine::Session;
use effectcraft_engine::config::{ConfigStore, MemoryConfig};
use effectcraft_ui_egui::EffectcraftApp;
use effectcraft_ui_egui::panels::timeline_columns::{self as tc, LAYOUT_FILE};
use egui::{Event, Modifiers, PointerButton, Pos2, Rect, pos2, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use serde_json::json;

fn session() -> (Session, u64) {
    let mut s = Session::default();
    s.execute("comp.new", json!({"name": "Columns", "width": 320, "height": 180, "frameRate": 30, "duration": 4})).unwrap();
    let id = s.execute("layer.newSolid", json!({"name": "Plate", "color": "#406080"})).unwrap()["layer"].as_u64().unwrap();
    (s, id)
}

fn launch(s: Session) -> Harness<'static, EffectcraftApp> {
    let mut h = Harness::builder().with_size(vec2(1600.0, 1000.0)).build_eframe(|_| EffectcraftApp::new(s));
    h.run_steps(3);
    h
}

fn harness() -> (Harness<'static, EffectcraftApp>, u64) {
    let (s, id) = session();
    (launch(s), id)
}

fn rect(h: &Harness<'_, EffectcraftApp>, id: &str) -> Rect {
    let e = h.state().auto.find(id).unwrap_or_else(|| panic!("no {id}")).clone();
    Rect::from_min_size(pos2(e.rect[0], e.rect[1]), vec2(e.rect[2], e.rect[3]))
}

/// Press at `from`, move to `to` in steps, release there.
fn drag(h: &mut Harness<'_, EffectcraftApp>, from: Pos2, to: Pos2) {
    h.event(Event::PointerMoved(from));
    h.step();
    h.event(Event::PointerButton { pos: from, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
    h.step();
    for k in 1..=10 {
        h.event(Event::PointerMoved(from + (to - from) * (k as f32 / 10.0)));
        h.step();
    }
    h.event(Event::PointerButton { pos: to, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
    h.run_steps(3);
}

fn click(h: &mut Harness<'_, EffectcraftApp>, p: Pos2, button: PointerButton) {
    for pressed in [true, false] {
        h.event(Event::PointerMoved(p));
        h.event(Event::PointerButton { pos: p, button, pressed, modifiers: Modifiers::NONE });
        h.step();
    }
    h.run_steps(3);
}

/// `TIMELINE_COLUMNS_SNAPSHOTS=<dir>` saves a picture of the window, to look at the result.
fn snapshot(h: &mut Harness<'_, EffectcraftApp>, name: &str) {
    if let Ok(dir) = std::env::var("TIMELINE_COLUMNS_SNAPSHOTS") {
        let _gpu = crate::gpu_lock();
        h.render().unwrap().save(format!("{dir}/{name}.png")).unwrap();
    }
}

fn width(h: &Harness<'_, EffectcraftApp>, col: &str) -> f32 {
    tc::column_width(&h.state().ui.timeline, col)
}

#[test]
fn dragging_a_header_divider_resizes_the_column() {
    let (mut h, layer) = harness();
    let name0 = width(&h, "name");
    let parent0 = rect(&h, "timeline.header.parent").min.x;
    let handle = rect(&h, "timeline.header.name.resize").center();
    drag(&mut h, handle, handle + vec2(80.0, 0.0));
    assert!((width(&h, "name") - (name0 + 80.0)).abs() < 1.0, "{} → {}", name0, width(&h, "name"));
    assert!((rect(&h, "timeline.header.name").width() - (name0 + 80.0)).abs() < 1.0);
    // The columns right of it move along, in the header and on the layer rows alike.
    let parent1 = rect(&h, "timeline.header.parent").min.x;
    assert!((parent1 - parent0 - 80.0).abs() < 1.0, "{parent0} → {parent1}");
    assert!((rect(&h, &format!("timeline.layer.{layer}.parent")).min.x - (parent1 + 20.0)).abs() < 1.0);
    // Not narrower than the column's minimum; a long name ends in "…" inside the column.
    h.state_mut().session.execute("layer.rename", json!({"layer": layer, "name": "A layer name far too long to fit a narrow name column"})).unwrap();
    let handle = rect(&h, "timeline.header.name.resize").center();
    drag(&mut h, handle, handle - vec2(600.0, 0.0));
    assert_eq!(width(&h, "name"), 80.0);
    snapshot(&mut h, "narrow-name");
    let col = rect(&h, "timeline.header.name");
    let shown = rect(&h, &format!("timeline.layer.{layer}.name"));
    assert!(shown.max.x <= col.max.x + 0.5, "the name stays in its column: {shown:?} in {col:?}");
    // The icon columns keep their width (no divider), as in After Effects.
    assert!(h.state().auto.find("timeline.header.av.resize").is_none());
    assert!(h.state().auto.find("timeline.header.switches.resize").is_none());
}

#[test]
fn context_menu_hides_and_shows_a_column() {
    let (mut h, _) = harness();
    assert!(tc::column_visible(&h.state().ui.timeline, "parent"));
    // Right-click the Parent & Link header ▸ Hide This.
    let p = rect(&h, "timeline.header.parent").center();
    click(&mut h, p, PointerButton::Secondary);
    h.get_by_label("Hide This").click();
    h.run_steps(3);
    assert!(!tc::column_visible(&h.state().ui.timeline, "parent"));
    assert!(h.state().auto.find("timeline.header.parent").is_none());
    // Right-click any header ▸ Columns ▸ Parent & Link shows it again.
    let p = rect(&h, "timeline.header.name").center();
    click(&mut h, p, PointerButton::Secondary);
    // The submenu button (the comp is named "Columns" too, in the Project panel).
    h.get_by_label("Columns ⏵").hover();
    h.run_steps(3);
    snapshot(&mut h, "columns-menu");
    // The submenu lists After Effects' columns in its order.
    let listed: Vec<String> = tc::COLUMNS_MENU.iter().map(|c| h.state().auto.find(&format!("timeline.columnsMenu.{c}")).unwrap().label.clone()).collect();
    assert_eq!(
        listed,
        ["A/V Features", "Label", "#", "Layer Name", "Comment", "Switches", "Modes", "Parent & Link", "Keys", "In", "Out", "Duration", "Stretch"]
    );
    h.get_by_label("Parent & Link").click();
    h.run_steps(3);
    assert!(tc::column_visible(&h.state().ui.timeline, "parent"));
    assert!(h.state().auto.find("timeline.header.parent").is_some());
}

#[test]
fn dragging_a_header_reorders_the_columns() {
    let (mut h, layer) = harness();
    let switches = rect(&h, "timeline.header.switches");
    let parent = rect(&h, "timeline.header.parent");
    assert!(switches.min.x < parent.min.x);
    // Drag Parent & Link onto the left half of Switches: it goes before it.
    drag(&mut h, parent.center(), pos2(switches.min.x + 6.0, switches.center().y));
    let (switches, parent) = (rect(&h, "timeline.header.switches"), rect(&h, "timeline.header.parent"));
    snapshot(&mut h, "reordered");
    assert!((parent.max.x - switches.min.x).abs() < 1.0, "Parent & Link now ends where Switches starts: {parent:?} {switches:?}");
    // The layer rows follow the headers.
    let row_parent = rect(&h, &format!("timeline.layer.{layer}.parent"));
    let row_shy = rect(&h, &format!("timeline.layer.{layer}.switch.shy"));
    assert!(row_parent.max.x <= row_shy.min.x && row_parent.min.x >= parent.min.x);
    // A click (no drag) on a header doesn't move anything.
    let order = h.state().ui.timeline.column_order.clone();
    click(&mut h, switches.center(), PointerButton::Primary);
    assert_eq!(h.state().ui.timeline.column_order, order);
    // Through the command, as agents do: to the right end.
    let ctx = h.ctx.clone();
    let r = effectcraft_ui_egui::menus::invoke(h.state_mut(), &ctx, "timeline.column", json!({"column": "parent", "before": null})).unwrap();
    assert_eq!(r["columns"].as_array().unwrap().last().unwrap(), "parent");
}

#[test]
fn clicking_the_name_header_toggles_source_and_layer_name() {
    let (mut h, layer) = harness();
    h.state_mut().session.execute("layer.rename", json!({"layer": layer, "name": "Renamed"})).unwrap();
    h.run_steps(2);
    let source = h.state().session.project.items.values().find(|i| matches!(i.kind, effectcraft_engine::project::ItemKind::Solid(_))).unwrap().name.clone();
    assert_ne!(source, "Renamed");
    assert_eq!(h.state().auto.find("timeline.header.name").unwrap().label, "Layer Name");
    assert_eq!(h.state().auto.find(&format!("timeline.layer.{layer}.name")).unwrap().label, "Renamed");
    let p = rect(&h, "timeline.header.name").center();
    click(&mut h, p, PointerButton::Primary);
    assert!(h.state().ui.timeline.source_name);
    assert_eq!(h.state().auto.find("timeline.header.name").unwrap().label, "Source Name");
    assert_eq!(h.state().auto.find(&format!("timeline.layer.{layer}.name")).unwrap().label, source);
    click(&mut h, p, PointerButton::Primary);
    assert!(!h.state().ui.timeline.source_name);
    assert_eq!(h.state().auto.find(&format!("timeline.layer.{layer}.name")).unwrap().label, "Renamed");
}

#[test]
fn the_splitter_resizes_the_outline_and_keeps_rows_aligned() {
    let (mut h, layer) = harness();
    assert_eq!(h.state().ui.timeline.outline_w, None);
    let ruler0 = rect(&h, "timeline.ruler").min.x;
    let s = rect(&h, "timeline.splitter").center();
    drag(&mut h, s, s + vec2(150.0, 0.0));
    assert!(h.state().ui.timeline.outline_w.is_some(), "the splitter sets the outline width");
    let ruler1 = rect(&h, "timeline.ruler").min.x;
    assert!((ruler1 - ruler0 - 150.0).abs() < 1.0, "the time graph starts 150 px further right: {ruler0} → {ruler1}");
    assert!((rect(&h, "timeline.splitter").center().x - (s.x + 150.0)).abs() < 1.0);
    // Row and bar stay on one line.
    let (row, bar) = (rect(&h, &format!("timeline.layer.{layer}.row")), rect(&h, &format!("timeline.layer.{layer}.bar")));
    assert!(bar.center().y >= row.min.y && bar.center().y <= row.max.y, "{row:?} {bar:?}");
    assert!(bar.min.x >= ruler1);
    // Narrower than the columns: they scroll; never narrower than the minimum.
    let s = rect(&h, "timeline.splitter").center();
    drag(&mut h, s, pos2(0.0, s.y));
    assert_eq!(h.state().ui.timeline.outline_w, Some(tc::MIN_OUTLINE_W));
    assert!(h.state().auto.find("timeline.outlineScroll").is_some());
    // Double-click: back to fitting the columns.
    let s = rect(&h, "timeline.splitter").center();
    h.event(Event::PointerMoved(s));
    h.step();
    // Both clicks in one frame (the harness' frames are further apart than a double-click).
    for _ in 0..2 {
        for pressed in [true, false] {
            h.input_mut().events.push(Event::PointerButton { pos: s, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE });
        }
    }
    h.run_steps(3);
    assert_eq!(h.state().ui.timeline.outline_w, None);
}

#[test]
fn pane_buttons_and_toggle_switches_modes() {
    let (mut h, layer) = harness();
    let tl = |h: &Harness<'_, EffectcraftApp>| h.state().ui.timeline.clone();
    // In/Out/Duration/Stretch: all four together.
    let p = rect(&h, "timeline.pane.inOut").center();
    click(&mut h, p, PointerButton::Primary);
    assert!(["in", "out", "duration", "stretch"].iter().all(|c| tc::column_visible(&tl(&h), c)));
    assert!(h.state().auto.find("timeline.header.stretch").is_some());
    snapshot(&mut h, "in-out-pane");
    // Stretch is a percentage already (it showed 10000.0%).
    assert_eq!(h.state().auto.find(&format!("timeline.layer.{layer}.stretch")).unwrap().label, "100.0%");
    click(&mut h, p, PointerButton::Primary);
    assert!(!["in", "out", "duration", "stretch"].iter().any(|c| tc::column_visible(&tl(&h), c)));
    // Toggle Switches / Modes swaps the two columns.
    assert!(tc::column_visible(&tl(&h), "switches") && !tl(&h).show_modes);
    let p = rect(&h, "timeline.toggleSwitchesModes").center();
    click(&mut h, p, PointerButton::Primary);
    assert!(!tc::column_visible(&tl(&h), "switches") && tl(&h).show_modes);
    click(&mut h, p, PointerButton::Primary);
    assert!(tc::column_visible(&tl(&h), "switches") && !tl(&h).show_modes);
    // The Transfer Controls pane button shows Modes beside the switches.
    let p = rect(&h, "timeline.pane.modes").center();
    click(&mut h, p, PointerButton::Primary);
    assert!(tc::column_visible(&tl(&h), "switches") && tl(&h).show_modes);
    let p = rect(&h, "timeline.pane.switches").center();
    click(&mut h, p, PointerButton::Primary);
    assert!(!tc::column_visible(&tl(&h), "switches"));
}

#[test]
fn the_column_layout_persists_across_launches() {
    let store = Arc::new(MemoryConfig::default());
    let start = || {
        let (mut s, _) = session();
        s.config = Some(store.clone());
        launch(s)
    };
    let mut h = start();
    assert!(store.read(LAYOUT_FILE).is_none(), "nothing written until the layout changes");
    let ctx = h.ctx.clone();
    effectcraft_ui_egui::menus::invoke(h.state_mut(), &ctx, "timeline.column", json!({"column": "comment", "visible": true})).unwrap();
    h.run_steps(2);
    let handle = rect(&h, "timeline.header.comment.resize").center();
    drag(&mut h, handle, handle + vec2(40.0, 0.0));
    let header = rect(&h, "timeline.header.label").center();
    let av = rect(&h, "timeline.header.av").min.x;
    drag(&mut h, header, pos2(av + 4.0, header.y));
    let p = rect(&h, "timeline.header.parent").center();
    click(&mut h, p, PointerButton::Secondary);
    h.get_by_label("Hide This").click();
    h.run_steps(3);
    let s = rect(&h, "timeline.splitter").center();
    drag(&mut h, s, s + vec2(60.0, 0.0));
    let name = rect(&h, "timeline.header.name").center();
    click(&mut h, name, PointerButton::Primary);
    let before = h.state().ui.timeline.clone();
    assert!(before.source_name);
    assert_eq!(tc::column_order(&before)[0], "label");
    assert_eq!(tc::column_width(&before, "comment"), 160.0);
    drop(h);

    let h = start();
    let after = &h.state().ui.timeline;
    assert_eq!(tc::column_order(after), tc::column_order(&before));
    assert_eq!(after.column_widths, before.column_widths);
    assert_eq!(after.columns, before.columns);
    assert!(!tc::column_visible(after, "parent") && tc::column_visible(after, "comment"));
    assert_eq!(after.outline_w, before.outline_w);
    assert!(after.source_name);
    assert!((rect(&h, "timeline.header.comment").width() - 160.0).abs() < 1.0);
    drop(h);

    // A damaged file is ignored.
    store.write(LAYOUT_FILE, "{not json").unwrap();
    let h = start();
    assert_eq!(tc::column_order(&h.state().ui.timeline)[0], "av");
    assert_eq!(h.state().ui.timeline.outline_w, None);
}
