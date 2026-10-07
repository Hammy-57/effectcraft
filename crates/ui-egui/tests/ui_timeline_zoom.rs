//! Zooming the Timeline's time ruler with real input: Alt+wheel zooms out until the whole comp
//! shows (#158); the Time Navigator's ends drag to zoom and `;` toggles frame level / the whole
//! comp (#159).

use effectcraft_engine::Session;
use effectcraft_ui_egui::EffectcraftApp;
use egui::{Event, Modifiers, MouseWheelUnit, Pos2, pos2, vec2};
use egui_kittest::Harness;
use serde_json::json;

/// A 10-minute comp with one solid, zoomed in on its start.
fn harness() -> (Harness<'static, EffectcraftApp>, u64) {
    let mut s = Session::default();
    s.execute("comp.new", json!({"name": "Long", "width": 320, "height": 180, "frameRate": 30, "duration": 600})).unwrap();
    let layer = s.execute("layer.newSolid", json!({"name": "Plate", "color": "#406080"})).unwrap()["layer"].as_u64().unwrap();
    let mut app = EffectcraftApp::new(s);
    app.ui.timeline.pps = Some(200.0);
    let mut h = Harness::builder().with_size(vec2(1600.0, 1000.0)).build_eframe(|_| app);
    h.run_steps(3);
    (h, layer)
}

/// A point over the layer's bar in the time graph.
fn over_bar(h: &Harness<'_, EffectcraftApp>, layer: u64) -> Pos2 {
    let e = h.state().auto.find(&format!("timeline.layer.{layer}.bar")).expect("layer bar").clone();
    pos2(e.rect[0] + 40.0, e.rect[1] + e.rect[3] / 2.0)
}

fn alt_wheel(h: &mut Harness<'_, EffectcraftApp>, at: Pos2, dy: f32) {
    h.event(Event::PointerMoved(at));
    h.event(Event::ModifiersChanged(Modifiers::ALT));
    h.event(Event::MouseWheel { unit: MouseWheelUnit::Point, delta: vec2(0.0, dy), modifiers: Modifiers::ALT, phase: egui::TouchPhase::Move });
    h.step();
    h.event(Event::ModifiersChanged(Modifiers::NONE));
    h.step();
}

/// #158: on a long comp, Alt+wheel zooms out all the way to the whole comp (and no further).
#[test]
fn alt_wheel_zooms_out_until_the_whole_comp_shows() {
    let (mut h, layer) = harness();
    for _ in 0..80 {
        let at = over_bar(&h, layer);
        alt_wheel(&mut h, at, -120.0);
        if h.state().ui.timeline.pps.is_none() {
            break;
        }
    }
    let tl = &h.state().ui.timeline;
    assert_eq!((tl.pps, tl.start), (None, 0.0), "fits the whole comp: {:?}", tl.pps);
    // The bar spans (nearly) the whole time graph.
    let bar = h.state().auto.find(&format!("timeline.layer.{layer}.bar")).unwrap().rect;
    let ruler = h.state().auto.find("timeline.ruler").unwrap().rect;
    assert!(bar[2] > ruler[2] * 0.95, "bar {bar:?} ruler {ruler:?}");
    // Zooming in again works from there.
    let at = over_bar(&h, layer);
    alt_wheel(&mut h, at, 240.0);
    assert!(h.state().ui.timeline.pps.is_some());
}

fn rect(h: &Harness<'_, EffectcraftApp>, id: &str) -> egui::Rect {
    let e = h.state().auto.find(id).unwrap_or_else(|| panic!("no {id}")).clone();
    egui::Rect::from_min_size(pos2(e.rect[0], e.rect[1]), vec2(e.rect[2], e.rect[3]))
}

fn drag(h: &mut Harness<'_, EffectcraftApp>, from: Pos2, to: Pos2) {
    h.event(Event::PointerMoved(from));
    h.step();
    h.event(Event::PointerButton { pos: from, button: egui::PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
    h.step();
    for k in 1..=8 {
        h.event(Event::PointerMoved(from + (to - from) * (k as f32 / 8.0)));
        h.step();
    }
    h.event(Event::PointerButton { pos: to, button: egui::PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
    h.run_steps(2);
}

/// The visible span (seconds) from the ruler's automation label ("start,pps") and width.
fn visible(h: &Harness<'_, EffectcraftApp>) -> (f64, f64) {
    let e = h.state().auto.find("timeline.ruler").unwrap().clone();
    let (start, pps) = e.label.split_once(',').map(|(a, b)| (a.parse::<f64>().unwrap(), b.parse::<f64>().unwrap())).unwrap();
    (start, start + (e.rect[2] - 16.0) as f64 / pps)
}

/// #159: dragging the navigator's end handles changes that side of the visible span; pulling
/// them out to the track's ends shows the whole comp.
#[test]
fn navigator_ends_drag_to_zoom() {
    let (mut h, _) = harness();
    h.state_mut().ui.timeline.pps = None;
    h.run_steps(3);
    let track = rect(&h, "timeline.navigator");
    // The end handle pulled to the middle: the span ends near half the comp, starts at 0.
    let end = rect(&h, "timeline.navigator.end").center();
    drag(&mut h, end, pos2(track.center().x, end.y));
    let (a, b) = visible(&h);
    assert!(a.abs() < 1e-6 && (b - 300.0).abs() < 15.0, "{a}..{b}");
    // The start handle pulled to a quarter: the span starts there, its end stays.
    let start = rect(&h, "timeline.navigator.start").center();
    drag(&mut h, start, pos2(track.min.x + track.width() * 0.25, start.y));
    let (a2, b2) = visible(&h);
    assert!((a2 - 150.0).abs() < 15.0 && (b2 - b).abs() < 2.0, "{a2}..{b2} (end was {b})");
    // Both ends back out to the track's ends: the whole comp.
    let start = rect(&h, "timeline.navigator.start").center();
    drag(&mut h, start, pos2(track.min.x - 20.0, start.y));
    let end = rect(&h, "timeline.navigator.end").center();
    drag(&mut h, end, pos2(track.max.x + 20.0, end.y));
    assert_eq!(h.state().ui.timeline.pps, None, "the whole comp");
}

/// #159: `;` zooms in to frame level around the current time, and back out to the whole comp.
#[test]
fn semicolon_toggles_frame_level_and_the_whole_comp() {
    let (mut h, _) = harness();
    h.state_mut().ui.timeline.pps = None;
    h.state_mut().session.execute("time.set", json!({"time": 120.0})).unwrap();
    h.run_steps(3);
    let ctx = h.ctx.clone();
    effectcraft_ui_egui::menus::invoke(h.state_mut(), &ctx, "timeline.zoomFrameToggle", json!({})).unwrap();
    h.run_steps(2);
    let (a, b) = visible(&h);
    assert!(a < 120.0 && 120.0 < b && b - a < 1.0, "frame level around the CTI: {a}..{b}");
    effectcraft_ui_egui::menus::invoke(h.state_mut(), &ctx, "timeline.zoomFrameToggle", json!({})).unwrap();
    assert_eq!(h.state().ui.timeline.pps, None);
}
