//! The Timeline's Render Time pane (After Effects' "Expand or Collapse the Render Time pane", the
//! footer's fourth button): each layer's render time on the viewer's frame as a bar and
//! milliseconds, a precomp's own layers included once it is opened. Before, the pane did not
//! exist and the renderer was never asked for the layers' times.

use effectcraft_engine::Session;
use effectcraft_engine::project::ItemId;
use effectcraft_ui_egui::EffectcraftApp;
use effectcraft_ui_egui::panels::timeline_columns as tc;
use egui::{Event, Modifiers, PointerButton, Pos2, Rect, pos2, vec2};
use egui_kittest::Harness;
use serde_json::json;

fn rect(h: &Harness<'_, EffectcraftApp>, id: &str) -> Rect {
    let e = h.state().auto.find(id).unwrap_or_else(|| panic!("no {id}")).clone();
    Rect::from_min_size(pos2(e.rect[0], e.rect[1]), vec2(e.rect[2], e.rect[3]))
}

fn click(h: &mut Harness<'_, EffectcraftApp>, p: Pos2) {
    h.input_mut().events.push(Event::PointerMoved(p));
    h.step();
    for pressed in [true, false] {
        h.input_mut().events.push(Event::PointerButton { pos: p, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE });
        h.step();
    }
    h.run_steps(2);
}

/// The Render Time cell of `layer`, once the viewer's frame has rendered: its ms.
fn render_time(h: &mut Harness<'_, EffectcraftApp>, layer: u64) -> f64 {
    let id = format!("timeline.layer.{layer}.renderTime");
    for _ in 0..600 {
        h.step();
        if let Some(e) = h.state().auto.find(&id) {
            let label = e.label.clone();
            let ms = label.strip_suffix("ms").and_then(|v| v.parse::<f64>().ok()).unwrap_or_else(|| panic!("`{label}` is not a time in ms"));
            return ms;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    panic!("no render time for layer {layer}");
}

#[test]
fn the_render_time_pane_shows_each_layers_time_on_the_viewer_frame() {
    let mut s = Session::default();
    s.execute("comp.new", json!({"name": "Main", "width": 320, "height": 180, "frameRate": 30, "duration": 4})).unwrap();
    let plate = s.execute("layer.newSolid", json!({"name": "Plate", "color": "#406080"})).unwrap()["layer"].as_u64().unwrap();
    s.execute("effect.apply", json!({"layers": [plate], "effect": "Gaussian Blur"})).unwrap();
    let red = s.execute("layer.newSolid", json!({"name": "Red", "color": "#c03030", "width": 80, "height": 60})).unwrap()["layer"].as_u64().unwrap();
    let r = s.execute("layer.precompose", json!({"layers": [red], "name": "Inner", "mode": "move", "open": false})).unwrap();
    let (inner, pre) = (r["comp"].as_u64().unwrap(), r["layer"].as_u64().unwrap());
    let inner_layer = s.project.comp(ItemId(inner)).and_then(|c| c.layers.iter().next().map(|l| l.id.0)).unwrap();
    let mut h = Harness::builder().with_size(vec2(1600.0, 1000.0)).build_eframe(|_| EffectcraftApp::new(s));
    h.run_steps(3);

    // The footer's fourth pane button shows the Render Time column (and hides it again).
    assert!(!tc::column_visible(&h.state().ui.timeline, "renderTime"));
    let button = rect(&h, "timeline.pane.renderTime").center();
    click(&mut h, button);
    assert!(tc::column_visible(&h.state().ui.timeline, "renderTime"));
    assert_eq!(h.state().auto.find("timeline.header.renderTime").map(|e| e.label.clone()).as_deref(), Some("Render Time"));

    // Every layer of the frame in the viewer has its time, the precomp layer included.
    for layer in [plate, pre] {
        let ms = render_time(&mut h, layer);
        assert!(ms.is_finite() && ms >= 0.0, "layer {layer}: {ms} ms");
    }
    let cell = rect(&h, &format!("timeline.layer.{plate}.renderTime"));
    let header = rect(&h, "timeline.header.renderTime");
    assert!(cell.min.x >= header.min.x - 1.0 && cell.max.x <= header.max.x + 1.0, "the cell sits in the Render Time column");

    // Opening the precomp shows its own layers' times.
    h.state_mut().session.execute("comp.open", json!({"comp": inner})).unwrap();
    h.run_steps(3);
    let ms = render_time(&mut h, inner_layer);
    assert!(ms.is_finite() && ms >= 0.0);

    click(&mut h, button);
    assert!(!tc::column_visible(&h.state().ui.timeline, "renderTime"));
    assert!(h.state().auto.find(&format!("timeline.layer.{inner_layer}.renderTime")).is_none());
}
