//! The menu bar from the keyboard, and the app's shortcuts waiting while a menu is open (#279)
//! (egui_kittest, UI logic only).

use effectcraft_engine::Session;
use effectcraft_ui_egui::EffectcraftApp;
use egui::{Event, Key, Modifiers, Pos2, Rect, pos2, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use serde_json::json;

fn harness() -> Harness<'static, EffectcraftApp> {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    let mut h = Harness::builder().with_size(vec2(1400.0, 900.0)).build_eframe(|_| EffectcraftApp::new(s));
    h.run_steps(3);
    h
}

fn rect(h: &Harness<'_, EffectcraftApp>, id: &str) -> Rect {
    let e = h.state().auto.find(id).unwrap_or_else(|| panic!("no {id}"));
    Rect::from_min_size(pos2(e.rect[0], e.rect[1]), vec2(e.rect[2], e.rect[3]))
}

fn click_at(h: &mut Harness<'_, EffectcraftApp>, p: Pos2) {
    h.input_mut().events.push(Event::PointerMoved(p));
    h.input_mut().events.push(Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed: true, modifiers: Default::default() });
    h.step();
    h.input_mut().events.push(Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed: false, modifiers: Default::default() });
    h.run_steps(2);
}

/// Press and release `key` (one frame each), then let the UI settle.
fn key(h: &mut Harness<'_, EffectcraftApp>, key: Key) {
    h.input_mut().events.push(Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE });
    h.step();
    h.input_mut().events.push(Event::Key { key, physical_key: None, pressed: false, repeat: false, modifiers: Modifiers::NONE });
    h.run_steps(3);
}

/// The label of the widget with keyboard focus (the highlighted menu entry).
fn focused(h: &Harness<'_, EffectcraftApp>) -> String {
    h.query_by(|n| n.is_focused()).and_then(|n| n.accesskit_node().label()).unwrap_or_default().trim().to_string()
}

fn shown(h: &Harness<'_, EffectcraftApp>, label: &str) -> bool {
    h.query_by_label_contains(label).is_some()
}

/// Space (or any shortcut) while a menu is open goes to the menu, not the app: it started the
/// preview behind the open menu.
#[test]
fn shortcuts_wait_while_a_menu_is_open() {
    let mut h = harness();
    let edit = rect(&h, "menu.Edit");
    click_at(&mut h, edit.center());
    assert!(shown(&h, "Keyboard Shortcuts"), "the Edit menu is open");
    key(&mut h, Key::Space);
    assert!(!h.state().playback.playing, "Space didn't start the preview");
    // With the menu closed, Space previews again.
    key(&mut h, Key::Escape);
    assert!(!shown(&h, "Keyboard Shortcuts"), "Escape closed the menu");
    key(&mut h, Key::Space);
    assert!(h.state().playback.playing, "Space previews with the menus closed");
}

/// Alt focuses the menu bar; Down opens the menu, Up / Down move the highlight, Right opens a
/// submenu and Left closes it, Right on an entry opens the next menu, Escape closes them.
#[test]
fn arrow_keys_drive_the_menu_bar() {
    if cfg!(target_os = "macos") {
        // (macOS menus are the system's.)
        return;
    }
    let mut h = harness();
    h.input_mut().events.push(Event::ModifiersChanged(Modifiers::ALT));
    h.step();
    h.input_mut().events.push(Event::ModifiersChanged(Modifiers::NONE));
    h.run_steps(2);
    assert_eq!(focused(&h), "File", "Alt focused the menu bar");
    key(&mut h, Key::ArrowDown);
    assert!(shown(&h, "Open Project"), "Down opened File");
    assert!(focused(&h).starts_with("New"), "its first entry is highlighted: {:?}", focused(&h));
    key(&mut h, Key::ArrowRight);
    assert!(focused(&h).contains("New Project"), "Right opened the New submenu: {:?}", focused(&h));
    key(&mut h, Key::ArrowDown);
    assert!(focused(&h).contains("New Project from Template"), "Down moved down the submenu: {:?}", focused(&h));
    key(&mut h, Key::ArrowLeft);
    assert!(!shown(&h, "New Project from Template"), "Left closed the submenu");
    assert!(focused(&h).starts_with("New"), "and went back to its entry: {:?}", focused(&h));
    key(&mut h, Key::ArrowDown);
    assert!(focused(&h).contains("Open Project"), "{:?}", focused(&h));
    key(&mut h, Key::ArrowRight);
    assert!(!shown(&h, "Open Project"), "Right on an entry left File");
    assert!(shown(&h, "Keyboard Shortcuts"), "for Edit");
    key(&mut h, Key::ArrowLeft);
    assert!(shown(&h, "Open Project"), "Left went back to File");
    key(&mut h, Key::Escape);
    assert!(!shown(&h, "Open Project"), "Escape closed the menus");
    assert!(!h.state().playback.playing, "no key reached the app");
}

/// Up in a menu opened with the pointer highlights its last entry (wrapping around); Enter on a
/// submenu entry opens it with its first entry highlighted, and Enter chooses an entry.
#[test]
fn enter_chooses_the_highlighted_entry() {
    let mut h = harness();
    let edit = rect(&h, "menu.Edit");
    click_at(&mut h, edit.center());
    key(&mut h, Key::ArrowUp);
    if cfg!(target_os = "macos") {
        assert!(focused(&h).starts_with("Keyboard Shortcuts"), "Up went to the last entry: {:?}", focused(&h));
        key(&mut h, Key::Enter);
        assert_eq!(h.state().dialog, Some(effectcraft_ui_egui::Dialog::Shortcuts), "Enter chose it");
    } else {
        assert!(focused(&h).starts_with("Preferences"), "Up went to the last entry: {:?}", focused(&h));
        key(&mut h, Key::Enter);
        assert!(focused(&h).starts_with("General"), "Enter opened the submenu: {:?}", focused(&h));
        key(&mut h, Key::Enter);
        assert_eq!(h.state().dialog, Some(effectcraft_ui_egui::Dialog::Settings), "Enter chose General...");
    }
    assert!(!shown(&h, "Quick Apply"), "and closed the menu");
}
