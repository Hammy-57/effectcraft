//! Trimming layers in the Timeline as in After Effects: the pointer over either end of a layer
//! bar (a few points inside it or just outside) shows the trim cursor, and dragging that end
//! moves the layer's In or Out point.
use effectcraft_engine::Session;
use effectcraft_engine::project::LayerId;
use effectcraft_ui_egui::EffectcraftApp;
use egui::{CursorIcon, Event, Modifiers, Pos2, pos2, vec2};
use egui_kittest::Harness;
use serde_json::json;

/// A 10 s comp at 30 fps with a solid trimmed to 2–6 s and a 20 s precomp trimmed to 3–7 s (its
/// source runs past both ends, so the slip zones sit right beside its bar's ends).
fn harness() -> (Harness<'static, EffectcraftApp>, u64, u64) {
    let mut s = Session::default();
    s.execute("comp.new", json!({"name": "Inner", "width": 320, "height": 180, "frameRate": 30, "duration": 20})).unwrap();
    s.execute("comp.new", json!({"name": "Main", "width": 320, "height": 180, "frameRate": 30, "duration": 10})).unwrap();
    let solid = s.execute("layer.newSolid", json!({"name": "Plate", "color": "#406080"})).unwrap()["layer"].as_u64().unwrap();
    s.execute("layer.timing", json!({"layers": [solid], "in": 2.0, "out": 6.0})).unwrap();
    let pre = s.execute("layer.addItem", json!({"item": "Inner"})).unwrap()["layer"].as_u64().unwrap();
    s.execute("layer.timing", json!({"layers": [pre], "in": 3.0, "out": 7.0})).unwrap();
    s.execute("layer.select", json!({"layers": []})).unwrap();
    let app = EffectcraftApp::new(s);
    let mut h = Harness::builder().with_size(vec2(1600.0, 1000.0)).build_eframe(|_| app);
    h.run_steps(4);
    (h, solid, pre)
}

fn bar(h: &Harness<'_, EffectcraftApp>, layer: u64) -> egui::Rect {
    let e = h.state().auto.find(&format!("timeline.layer.{layer}.bar")).expect("layer bar").clone();
    egui::Rect::from_min_size(pos2(e.rect[0], e.rect[1]), vec2(e.rect[2], e.rect[3]))
}

/// The cursor shown with the pointer resting at `p`.
fn cursor_at(h: &mut Harness<'_, EffectcraftApp>, p: Pos2) -> CursorIcon {
    h.event(Event::PointerMoved(p));
    h.run_steps(3);
    h.output().platform_output.cursor_icon
}

/// (in, out, start) of `layer` in seconds.
fn timing(h: &Harness<'_, EffectcraftApp>, layer: u64) -> (f64, f64, f64) {
    let comp = h.state().session.active_comp().unwrap();
    let l = comp.layer(LayerId(layer)).unwrap();
    (l.in_point.seconds(), l.out_point.seconds(), l.start_time.seconds())
}

fn drag(h: &mut Harness<'_, EffectcraftApp>, from: Pos2, to: Pos2) {
    h.event(Event::PointerMoved(from));
    h.step();
    h.event(Event::PointerButton { pos: from, button: egui::PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
    h.step();
    for k in 1..=10 {
        h.event(Event::PointerMoved(from + (to - from) * (k as f32 / 10.0)));
        h.step();
    }
    h.event(Event::PointerButton { pos: to, button: egui::PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
    h.run_steps(3);
}

/// Both ends of every bar show the trim cursor, just inside and just outside the end; the middle
/// doesn't. The same holds for a selected layer, a twirled-open layer and a short bar.
fn assert_trim_cursor_at_ends(h: &mut Harness<'_, EffectcraftApp>, layer: u64, case: &str) {
    let b = bar(h, layer);
    let y = b.center().y;
    for (what, x) in [("in, outside", b.min.x - 3.0), ("in, inside", b.min.x + 2.0), ("out, inside", b.max.x - 2.0), ("out, outside", b.max.x + 3.0)] {
        assert_eq!(cursor_at(h, pos2(x, y)), CursorIcon::ResizeHorizontal, "{case}: layer {layer} {what} (bar {b:?})");
    }
    if b.width() > 40.0 {
        assert_ne!(cursor_at(h, b.center()), CursorIcon::ResizeHorizontal, "{case}: the middle of the bar moves, it doesn't trim");
    }
}

#[test]
fn bar_ends_show_the_trim_cursor() {
    let (mut h, solid, pre) = harness();
    assert_trim_cursor_at_ends(&mut h, solid, "unselected solid");
    assert_trim_cursor_at_ends(&mut h, pre, "precomp with slip zones beside its ends");
    h.state_mut().session.execute("layer.select", json!({"layers": [solid]})).unwrap();
    h.run_steps(2);
    assert_trim_cursor_at_ends(&mut h, solid, "selected");
    h.state_mut().ui.timeline.open_layers.insert(solid);
    h.run_steps(2);
    assert_trim_cursor_at_ends(&mut h, solid, "twirled open");
    assert_trim_cursor_at_ends(&mut h, pre, "below a twirled-open layer");
    // Three frames long: still trims from both ends.
    h.state_mut().session.execute("layer.timing", json!({"layers": [solid], "in": 2.0, "out": 2.1})).unwrap();
    h.run_steps(2);
    assert_trim_cursor_at_ends(&mut h, solid, "short bar");
}

/// With the owner-local After Effects cursor installed (§1.9), the trim zones show it, with its
/// hotspot at the arrow's centre; without it, the system resize cursor alone.
#[test]
fn trim_cursor_uses_owner_artwork_when_installed() {
    let (mut h, solid, _) = harness();
    let b = bar(&h, solid);
    assert_eq!(cursor_at(&mut h, pos2(b.max.x, b.center().y)), CursorIcon::ResizeHorizontal);
    // Where `local_assets` looks: the override's folder, else (macOS) the owner-asset folder.
    let atlas = match std::env::var_os("EFFECTCRAFT_ICON_REFERENCE") {
        Some(path) => Some(std::path::PathBuf::from(path)),
        None if cfg!(target_os = "macos") => {
            std::env::var_os("HOME").map(|home| std::path::PathBuf::from(home).join("Library/Application Support/EffectCraft/owner-assets/native-icons.alpha"))
        }
        None => None,
    };
    let installed = atlas.and_then(|a| Some(a.parent()?.join("extra/cursor-trim.png"))).is_some_and(|p| p.exists());
    let image = h.output().platform_output.cursor_image.clone();
    if installed {
        let image = image.expect("the installed trim cursor shows");
        let scale = if h.ctx.pixels_per_point() >= 1.5 { 2 } else { 1 };
        assert_eq!(image.hotspot, [7 * scale, 3 * scale]);
        assert_eq!(image.rgba.len(), usize::from(image.size[0]) * usize::from(image.size[1]) * 4);
    } else {
        assert!(image.is_none(), "no custom cursor without the owner artwork");
    }
    // Leaving the bar puts the normal cursor back.
    cursor_at(&mut h, b.center());
    assert!(h.output().platform_output.cursor_image.is_none(), "trim cursor leaked onto the bar body");
}

#[test]
fn dragging_bar_ends_trims_in_and_out() {
    let (mut h, solid, pre) = harness();
    let b = bar(&h, solid);
    let pps = b.width() as f64 / 4.0;
    let y = b.center().y;
    // Out end, grabbed just inside, one second to the left.
    drag(&mut h, pos2(b.max.x - 2.0, y), pos2(b.max.x - 2.0 - pps as f32, y));
    let (i, o, s) = timing(&h, solid);
    assert!((i - 2.0).abs() < 1e-6 && (o - 5.0).abs() < 1.5 / 30.0 && s.abs() < 1e-6, "out trimmed to ~5 s: in {i} out {o} start {s}");
    // In end, grabbed just inside, one second to the right.
    let b = bar(&h, solid);
    drag(&mut h, pos2(b.min.x + 2.0, y), pos2(b.min.x + 2.0 + pps as f32, y));
    let (i, o2, s) = timing(&h, solid);
    assert!((i - 3.0).abs() < 1.5 / 30.0 && (o2 - o).abs() < 1e-6 && s.abs() < 1e-6, "in trimmed to ~3 s: in {i} out {o2} start {s}");
    // The precomp's Out end grabbed just outside the bar (over its slip zone) extends it.
    let b = bar(&h, pre);
    let y = b.center().y;
    drag(&mut h, pos2(b.max.x + 3.0, y), pos2(b.max.x + 3.0 + pps as f32, y));
    let (i, o, s) = timing(&h, pre);
    assert!((i - 3.0).abs() < 1e-6 && (o - 8.0).abs() < 1.5 / 30.0 && s.abs() < 1e-6, "out extended to ~8 s, not slipped: in {i} out {o} start {s}");
}

/// Trimming a selected layer's end trims every selected layer by the same amount (one undo
/// step), as in After Effects; an unselected layer's end trims that layer alone.
#[test]
fn dragging_a_selected_bar_end_trims_every_selected_layer() {
    let (mut h, solid, pre) = harness();
    h.state_mut().session.execute("layer.select", json!({"layers": [solid, pre]})).unwrap();
    h.run_steps(2);
    let b = bar(&h, pre);
    let pps = bar(&h, solid).width() as f64 / 4.0;
    let y = b.center().y;
    drag(&mut h, pos2(b.max.x - 2.0, y), pos2(b.max.x - 2.0 + pps as f32, y));
    let ((si, so, _), (pi, po, _)) = (timing(&h, solid), timing(&h, pre));
    assert!((so - 7.0).abs() < 1.5 / 30.0 && (po - 8.0).abs() < 1.5 / 30.0, "both outs ~+1 s: solid {so}, precomp {po}");
    assert!((so - 6.0 - (po - 7.0)).abs() < 1e-9, "the same delta for both");
    assert!((si - 2.0).abs() < 1e-9 && (pi - 3.0).abs() < 1e-9, "in points untouched");
    // The In end too.
    let b = bar(&h, solid);
    drag(&mut h, pos2(b.min.x + 2.0, b.center().y), pos2(b.min.x + 2.0 + pps as f32 / 2.0, b.center().y));
    let ((si, _, _), (pi, _, _)) = (timing(&h, solid), timing(&h, pre));
    assert!((si - 2.5).abs() < 1.5 / 30.0 && (si - 2.0 - (pi - 3.0)).abs() < 1e-9, "both ins ~+0.5 s: solid {si}, precomp {pi}");
    // One undo step per drag.
    h.state_mut().session.undo();
    h.state_mut().session.undo();
    h.run_steps(2);
    assert_eq!((timing(&h, solid), timing(&h, pre)), ((2.0, 6.0, 0.0), (3.0, 7.0, 0.0)));
    // An unselected layer's end: only it.
    h.state_mut().session.execute("layer.select", json!({"layers": [solid]})).unwrap();
    h.run_steps(2);
    let b = bar(&h, pre);
    drag(&mut h, pos2(b.max.x - 2.0, y), pos2(b.max.x - 2.0 + pps as f32, y));
    assert_eq!(timing(&h, solid), (2.0, 6.0, 0.0), "the selected layer stays");
    assert!((timing(&h, pre).1 - 8.0).abs() < 1.5 / 30.0);
}
