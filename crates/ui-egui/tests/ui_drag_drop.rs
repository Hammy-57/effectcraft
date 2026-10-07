//! Drag and drop between panels (egui_kittest, real pointer drags): Project
//! items land in the Timeline where they are dropped, between layers and, over the time graph,
//! starting there (#89); dropped on the Composition viewer they are centred where they land
//! (#85); an effect dropped on the viewer goes on the layer under the pointer (#88).

use effectcraft_engine::Session;
use effectcraft_engine::keyframe::Value as KV;
use effectcraft_engine::time::Tick;
use effectcraft_ui_egui::EffectcraftApp;
use effectcraft_ui_egui::dock::PanelKind;
use effectcraft_ui_egui::panels::viewer::{comp_to_screen, last_fit};
use egui::{Event, Modifiers, Pos2, Rect, pos2, vec2};
use egui_kittest::Harness;
use serde_json::json;

/// Comp "Main" (4 s at 30 fps, current time 1 s) with solids Top, Middle and Bottom, and comp
/// "Clip" in the Project panel to drag in.
fn harness() -> (Harness<'static, EffectcraftApp>, u64) {
    let mut s = Session::default();
    let clip = s.execute("comp.new", json!({"name": "Clip", "width": 160, "height": 90, "frameRate": 30, "duration": 1})).unwrap()["comp"].as_u64().unwrap();
    s.execute("comp.new", json!({"name": "Main", "width": 320, "height": 180, "frameRate": 30, "duration": 4})).unwrap();
    for name in ["Bottom", "Middle", "Top"] {
        s.execute("layer.newSolid", json!({"name": name, "color": "#406080"})).unwrap();
    }
    s.execute("time.set", json!({"time": 1.0})).unwrap();
    let mut h = Harness::builder().with_size(vec2(1600.0, 1000.0)).build_eframe(|_| EffectcraftApp::new(s));
    h.run_steps(3);
    (h, clip)
}

fn rect(h: &Harness<'_, EffectcraftApp>, id: &str) -> Rect {
    let e = h.state().auto.find(id).unwrap_or_else(|| panic!("no {id}")).clone();
    Rect::from_min_size(pos2(e.rect[0], e.rect[1]), vec2(e.rect[2], e.rect[3]))
}

/// Press on `from`, move to `to` in steps, release there (with `modifiers` held).
fn drag(h: &mut Harness<'_, EffectcraftApp>, from: Pos2, to: Pos2, modifiers: Modifiers) {
    h.event(Event::PointerMoved(from));
    h.step();
    h.event(Event::PointerButton { pos: from, button: egui::PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
    h.step();
    h.event(Event::ModifiersChanged(modifiers));
    for k in 1..=10 {
        h.event(Event::PointerMoved(from + (to - from) * (k as f32 / 10.0)));
        h.step();
    }
    h.event(Event::PointerButton { pos: to, button: egui::PointerButton::Primary, pressed: false, modifiers });
    h.run_steps(3);
    h.event(Event::ModifiersChanged(Modifiers::NONE));
    h.step();
}

/// Layer names top to bottom, and the In point of `name`.
fn stack(h: &Harness<'_, EffectcraftApp>) -> Vec<String> {
    h.state().session.active_comp().unwrap().layers.iter().map(|l| l.name.clone()).collect()
}

fn in_point(h: &Harness<'_, EffectcraftApp>, index: usize) -> f64 {
    h.state().session.active_comp().unwrap().layers[index].in_point.seconds()
}

fn layer_id(h: &Harness<'_, EffectcraftApp>, name: &str) -> u64 {
    h.state().session.active_comp().unwrap().layers.iter().find(|l| l.name == name).unwrap().id.0
}

/// #89: dropped on the layer outline, an item goes in between the layers there; over the time
/// graph it also starts where it was dropped, or at the current time with Shift.
#[test]
fn project_items_land_where_they_are_dropped_in_the_timeline() {
    let (mut h, clip) = harness();
    let item = rect(&h, &format!("project.item.{clip}.name")).center();
    let frame = 1.0 / 30.0;

    // Between Top and Middle in the outline: the lower half of Top's row.
    let top = rect(&h, &format!("timeline.layer.{}.row", layer_id(&h, "Top")));
    drag(&mut h, item, pos2(top.center().x, top.max.y - 2.0), Modifiers::NONE);
    assert_eq!(stack(&h), ["Top", "Clip", "Middle", "Bottom"]);
    // (Settings ▸ General ▸ Create Layers at Composition Start Time, on by default.)
    assert_eq!(in_point(&h, 1), 0.0, "starts where new layers start");
    h.state_mut().session.execute("edit.undo", json!({})).unwrap();
    h.run_steps(2);

    // Over the time graph at 2 s, on the upper half of Bottom's row: above Bottom, starting at 2 s.
    let bottom = layer_id(&h, "Bottom");
    let row = rect(&h, &format!("timeline.layer.{bottom}.row"));
    let bar = rect(&h, &format!("timeline.layer.{bottom}.bar"));
    let at_2s = bar.min.x + bar.width() * 0.5;
    drag(&mut h, item, pos2(at_2s, row.min.y + 2.0), Modifiers::NONE);
    assert_eq!(stack(&h), ["Top", "Middle", "Clip", "Bottom"]);
    assert!((in_point(&h, 2) - 2.0).abs() <= frame + 1e-9, "starts where it was dropped: {}", in_point(&h, 2));
    assert_eq!(
        Tick::from_seconds_f64(in_point(&h, 2)),
        h.state().session.active_comp().unwrap().frame_rate.snap_nearest(Tick::from_seconds_f64(in_point(&h, 2))),
        "on a frame"
    );
    h.state_mut().session.execute("edit.undo", json!({})).unwrap();
    h.run_steps(2);

    // Shift: at the current time; below the last layer: at the bottom of the stack.
    let row = rect(&h, &format!("timeline.layer.{bottom}.row"));
    drag(&mut h, item, pos2(at_2s, row.max.y + 30.0), Modifiers::SHIFT);
    assert_eq!(stack(&h), ["Top", "Middle", "Bottom", "Clip"]);
    assert!((in_point(&h, 3) - 1.0).abs() < 1e-9, "Shift starts it at the current time");
}

/// #85: a Project item dropped on the Composition viewer becomes a layer centred where it was
/// dropped (above the selected layer, like any new layer).
#[test]
fn project_items_dropped_on_the_viewer_land_under_the_pointer() {
    let (mut h, clip) = harness();
    let item = rect(&h, &format!("project.item.{clip}.name")).center();
    let to = comp_to_screen(&h.ctx, [80.0, 45.0]).unwrap();
    drag(&mut h, item, to, Modifiers::NONE);
    assert_eq!(stack(&h), ["Clip", "Top", "Middle", "Bottom"]);
    let comp = h.state().session.active_comp().unwrap();
    let Some(KV::Vec3(p)) = comp.layers[0].props.prop("transform/position").map(|p| p.value.clone()) else { panic!("no position") };
    // One screen point is up to 1/zoom comp pixels.
    let tol = 1.0 / last_fit(&h.ctx) as f64 + 1e-6;
    assert!((p[0] - 80.0).abs() <= tol && (p[1] - 45.0).abs() <= tol, "{p:?}");
}

/// #88: an effect dragged from Effects & Presets onto the viewer goes on the layer under the
/// pointer, not the selected one.
#[test]
fn effects_dropped_on_the_viewer_go_on_the_layer_under_the_pointer() {
    let (mut h, _) = harness();
    let (top, middle) = (layer_id(&h, "Top"), layer_id(&h, "Middle"));
    h.state_mut().session.execute("layer.select", json!({"layers": [middle]})).unwrap();
    h.state_mut().show_panel(PanelKind::EffectsPresets);
    h.state_mut().ui.effects_search = "Gaussian Blur".into();
    h.run_steps(3);
    let fx = rect(&h, "effects.item.ec.blur.gaussian").center();
    let to = comp_to_screen(&h.ctx, [160.0, 90.0]).unwrap();
    drag(&mut h, fx, to, Modifiers::NONE);
    let comp = h.state().session.active_comp().unwrap();
    let effects = |id: u64| comp.layer(effectcraft_engine::project::LayerId(id)).unwrap().effects().map_or(0, |fx| fx.groups().count());
    assert_eq!((effects(top), effects(middle)), (1, 0), "on Top, which is under the pointer");
}
