//! Shared draft editor for timeline and Effect Controls gradient properties.
use effectcraft_engine::keyframe::{Gradient, Value};
use egui::{Color32, Id, Rect, Sense, pos2, vec2};

use crate::{EffectcraftApp, automation::Registry};

/// Opening and cancelling never touch the document; Apply produces one `prop.set` value.
pub(super) fn show(app: &mut EffectcraftApp, ui: &mut egui::Ui, id: Id, rect: Rect, gradient: &Gradient) -> Option<Value> {
    let draft_id = id.with("draft");
    if ui.interact(rect, id, Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
        ui.data_mut(|d| d.insert_temp(draft_id, gradient.clone()));
    }
    let mut draft = ui.data(|d| d.get_temp::<Gradient>(draft_id))?;
    let mut open = true;
    let mut apply = false;
    let mut cancel = false;
    egui::Window::new("Edit Gradient").id(id.with("window")).open(&mut open).resizable(false).collapsible(false).default_width(420.0).show(ui.ctx(), |ui| {
        let (preview, _) = ui.allocate_exact_size(vec2(400.0, 24.0), Sense::hover());
        for k in 0..100 {
            let c = draft.sample(k as f64 / 99.0);
            let color = Color32::from_rgba_unmultiplied(
                (c[0].clamp(0.0, 1.0) * 255.0) as u8,
                (c[1].clamp(0.0, 1.0) * 255.0) as u8,
                (c[2].clamp(0.0, 1.0) * 255.0) as u8,
                (c[3].clamp(0.0, 1.0) * 255.0) as u8,
            );
            let r = Rect::from_min_max(pos2(preview.left() + k as f32 * 4.0, preview.top()), pos2(preview.left() + (k + 1) as f32 * 4.0, preview.bottom()));
            ui.painter().rect_filled(r, 0.0, if k % 2 == 0 { Color32::GRAY } else { Color32::LIGHT_GRAY });
            ui.painter().rect_filled(r, 0.0, color);
        }
        egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
            ui.label("Color stops · position / color / midpoint to next stop");
            let mut remove = None;
            let colors_len = draft.colors.len();
            for i in 0..colors_len {
                let bounds = bounds(&draft.colors, i);
                let Some((position, color)) = draft.colors.get_mut(i) else { continue };
                ui.push_id(("color", i), |ui| {
                    ui.horizontal(|ui| {
                        field(&mut app.auto, ui.add(egui::DragValue::new(position).range(bounds).speed(0.005)), &format!("gradient.color.{i}.position"));
                        field(&mut app.auto, ui.color_edit_button_rgba_unmultiplied(color), &format!("gradient.color.{i}.color"));
                        midpoint(ui, &mut app.auto, &mut draft.color_midpoints, i, colors_len, "color");
                        let r = ui.add_enabled(colors_len > 2, egui::Button::new("Remove"));
                        if r.clicked() {
                            remove = Some(i);
                        }
                        field(&mut app.auto, r, &format!("gradient.color.{i}.remove"));
                    })
                });
            }
            if let Some(i) = remove {
                remove_stop(&mut draft.colors, &mut draft.color_midpoints, i);
            }
            let r = ui.add_enabled(draft.colors.len() < 256, egui::Button::new("Add color stop"));
            if r.clicked() {
                add_stop(&mut draft.colors, &mut draft.color_midpoints, [1.0; 4]);
            }
            field(&mut app.auto, r, "gradient.color.add");
            ui.separator();
            ui.label("Opacity stops · position / opacity / midpoint to next stop");
            let mut remove = None;
            let opacity_len = draft.opacities.len();
            for i in 0..opacity_len {
                let bounds = bounds(&draft.opacities, i);
                let Some((position, alpha)) = draft.opacities.get_mut(i) else { continue };
                ui.push_id(("opacity", i), |ui| {
                    ui.horizontal(|ui| {
                        field(&mut app.auto, ui.add(egui::DragValue::new(position).range(bounds).speed(0.005)), &format!("gradient.opacity.{i}.position"));
                        field(&mut app.auto, ui.add(egui::DragValue::new(alpha).range(0.0..=1.0).speed(0.005)), &format!("gradient.opacity.{i}.alpha"));
                        midpoint(ui, &mut app.auto, &mut draft.opacity_midpoints, i, opacity_len, "opacity");
                        let r = ui.add_enabled(opacity_len > 1, egui::Button::new("Remove"));
                        if r.clicked() {
                            remove = Some(i);
                        }
                        field(&mut app.auto, r, &format!("gradient.opacity.{i}.remove"));
                    })
                });
            }
            if let Some(i) = remove {
                remove_stop(&mut draft.opacities, &mut draft.opacity_midpoints, i);
            }
            let r = ui.add_enabled(draft.opacities.len() < 256, egui::Button::new("Add opacity stop"));
            if r.clicked() {
                add_stop(&mut draft.opacities, &mut draft.opacity_midpoints, 1.0);
            }
            field(&mut app.auto, r, "gradient.opacity.add");
        });
        ui.separator();
        ui.horizontal(|ui| {
            let r = ui.button("Apply");
            apply = r.clicked();
            field(&mut app.auto, r, "gradient.apply");
            let r = ui.button("Cancel");
            cancel = r.clicked();
            field(&mut app.auto, r, "gradient.cancel");
        });
    });
    if !open || cancel || apply || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        ui.data_mut(|d| d.remove::<Gradient>(draft_id));
        return apply.then_some(Value::Gradient(Box::new(draft)));
    }
    ui.data_mut(|d| d.insert_temp(draft_id, draft));
    None
}

fn field(auto: &mut Registry, r: egui::Response, id: &str) {
    auto.add(id, r.rect, id);
}

// Keep neighboring stops ordered without detaching their authored midpoints.
fn bounds<T>(stops: &[(f64, T)], i: usize) -> std::ops::RangeInclusive<f64> {
    let min = i.checked_sub(1).and_then(|i| stops.get(i)).map(|s| s.0).filter(|p| p.is_finite()).unwrap_or(0.0).clamp(0.0, 1.0);
    let max = stops.get(i.saturating_add(1)).map(|s| s.0).filter(|p| p.is_finite()).unwrap_or(1.0).clamp(min, 1.0);
    min..=max
}

fn midpoint(ui: &mut egui::Ui, auto: &mut Registry, points: &mut Vec<f64>, i: usize, len: usize, kind: &str) {
    if i.saturating_add(1) >= len {
        return;
    }
    let mut value = points.get(i).copied().unwrap_or(0.5);
    let r = ui.add(egui::DragValue::new(&mut value).range(0.001..=0.999).speed(0.005));
    if r.changed() && i < 256 {
        points.resize(points.len().max(i.saturating_add(1)), 0.5);
        if let Some(p) = points.get_mut(i) {
            *p = value;
        }
    }
    field(auto, r, &format!("gradient.{kind}.{i}.midpoint"));
}

fn remove_stop<T>(stops: &mut Vec<(f64, T)>, midpoints: &mut Vec<f64>, i: usize) {
    if i < stops.len() {
        stops.remove(i);
    }
    if i < midpoints.len() {
        midpoints.remove(i);
    }
    midpoints.truncate(stops.len().saturating_sub(1));
}

fn add_stop<T>(stops: &mut Vec<(f64, T)>, midpoints: &mut Vec<f64>, value: T) {
    if stops.len() >= 256 {
        return;
    }
    let i = stops.iter().position(|s| s.0 > 0.5).unwrap_or(stops.len());
    stops.insert(i, (0.5, value));
    if !midpoints.is_empty() {
        midpoints.resize(stops.len().saturating_sub(2), 0.5);
        midpoints.insert(i.min(midpoints.len()), 0.5);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use effectcraft_engine::Session;
    use serde_json::json;

    fn frame(app: &mut EffectcraftApp, ctx: &egui::Context, events: Vec<egui::Event>, value: &Gradient) -> Option<Value> {
        let mut result = None;
        app.auto.begin_frame();
        let mut output =
            ctx.run_ui(egui::RawInput { screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(900.0, 700.0))), events, ..Default::default() }, |ui| {
                result = show(app, ui, Id::new("test-gradient"), Rect::from_min_size(pos2(10.0, 10.0), vec2(100.0, 20.0)), value);
            });
        output.textures_delta.clear();
        result
    }

    fn click(app: &mut EffectcraftApp, ctx: &egui::Context, value: &Gradient, pos: egui::Pos2) -> Option<Value> {
        frame(app, ctx, vec![egui::Event::PointerMoved(pos)], value);
        frame(app, ctx, vec![egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: true, modifiers: Default::default() }], value);
        let result = frame(
            app,
            ctx,
            vec![egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: false, modifiers: Default::default() }],
            value,
        );
        frame(app, ctx, vec![], value);
        result
    }

    fn button(app: &mut EffectcraftApp, ctx: &egui::Context, value: &Gradient, name: &str) -> Option<Value> {
        let r = app.auto.elements.iter().find(|e| e.id == name).unwrap().rect;
        click(app, ctx, value, pos2(r[0] + r[2] / 2.0, r[1] + r[3] / 2.0))
    }

    #[test]
    fn pointer_opens_edits_cancels_and_applies_gradient() {
        let mut app = EffectcraftApp::new(Session::default());
        let ctx = egui::Context::default();
        let initial = Gradient { color_midpoints: vec![0.25], opacity_midpoints: vec![0.75], ..Default::default() };
        frame(&mut app, &ctx, vec![], &initial);
        click(&mut app, &ctx, &initial, pos2(50.0, 20.0));
        button(&mut app, &ctx, &initial, "gradient.color.add");
        assert!(button(&mut app, &ctx, &initial, "gradient.cancel").is_none());
        assert!(ctx.data(|d| d.get_temp::<Gradient>(Id::new("test-gradient").with("draft"))).is_none());
        click(&mut app, &ctx, &initial, pos2(50.0, 20.0));
        button(&mut app, &ctx, &initial, "gradient.color.add");
        button(&mut app, &ctx, &initial, "gradient.opacity.add");
        let Value::Gradient(edited) = button(&mut app, &ctx, &initial, "gradient.apply").unwrap() else { panic!("expected gradient") };
        assert_eq!(edited.colors.len(), 3);
        assert_eq!(edited.opacities.len(), 3);
        assert_eq!(edited.colors[1].0, 0.5);
        assert_eq!(edited.color_midpoints[0], 0.25);
        assert_eq!(initial.colors.len(), 2);
    }

    #[test]
    fn stop_edits_preserve_order_and_midpoint_alignment() {
        let mut g = Gradient { color_midpoints: vec![0.25], ..Default::default() };
        add_stop(&mut g.colors, &mut g.color_midpoints, [0.3; 4]);
        assert_eq!(bounds(&g.colors, 1), 0.0..=1.0);
        assert_eq!(g.color_midpoints, vec![0.25, 0.5]);
        remove_stop(&mut g.colors, &mut g.color_midpoints, 1);
        assert_eq!(g.color_midpoints, vec![0.25]);
        assert_eq!(g.colors, Gradient::default().colors);
        assert_eq!(bounds(&[(f64::NAN, ()), (f64::INFINITY, ())], 1), 0.0..=1.0);
    }

    #[test]
    fn both_panel_entry_points_open_the_editor() {
        use effectcraft_engine::project::ParamUi;
        for timeline in [true, false] {
            let mut s = Session::default();
            s.execute("comp.new", json!({"width": 100, "height": 100, "duration": 1})).unwrap();
            s.execute("layer.newSolid", json!({})).unwrap();
            let lid = s.active_comp().unwrap().layers.last().unwrap().id;
            s.execute("effect.apply", json!({"layers": [lid.0], "effect": "Color Control"})).unwrap();
            let cid = s.active_comp_id().unwrap();
            let layer = s.project.comp_mut(cid).unwrap().layer_mut(lid).unwrap();
            let prop = layer.props.prop_mut("effects/#1/color").unwrap();
            prop.value = Value::gradient(Gradient::default());
            prop.ui = ParamUi::Gradient;
            let uid = prop.uid;
            s.state.selected_layers = vec![lid];
            let mut app = EffectcraftApp::new(s);
            app.ui.timeline.search = "Color".into();
            let ctx = egui::Context::default();
            let panel_frame = |app: &mut EffectcraftApp, events| {
                app.auto.begin_frame();
                let mut out = ctx.run_ui(
                    egui::RawInput { screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(1000.0, 800.0))), events, ..Default::default() },
                    |ui| {
                        let rect = Rect::from_min_size(egui::Pos2::ZERO, vec2(1000.0, 800.0));
                        if timeline {
                            super::super::timeline::show(app, ui, rect);
                        } else {
                            super::super::effect_controls::show(app, ui, rect);
                        }
                    },
                );
                out.textures_delta.clear();
            };
            panel_frame(&mut app, vec![]);
            panel_frame(&mut app, vec![]);
            let prefix = if timeline { "timeline" } else { "effectControls" };
            let id = format!("{prefix}.prop.{uid}.value");
            let r = app.auto.elements.iter().find(|e| e.id == id).unwrap().rect;
            let pos = pos2(r[0] + r[2] / 2.0, r[1] + r[3] / 2.0);
            panel_frame(&mut app, vec![egui::Event::PointerMoved(pos)]);
            for pressed in [true, false] {
                panel_frame(&mut app, vec![egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() }]);
            }
            panel_frame(&mut app, vec![]);
            assert!(app.auto.elements.iter().any(|e| e.id == "gradient.apply"), "{prefix} did not open the gradient editor");
        }
    }

    #[test]
    fn tagged_gradient_command_is_undoable_without_losing_alpha_or_midpoints() {
        let mut s = Session::default();
        s.execute("comp.new", json!({"width": 100, "height": 100, "duration": 1})).unwrap();
        s.execute("layer.newShape", json!({})).unwrap();
        let layer = s.active_comp().unwrap().layers.last().unwrap().id.0;
        s.execute("layer.addShapeItem", json!({"layer": layer, "kind": "gradientFill"})).unwrap();
        let prop = s.active_comp().unwrap().layers.last().unwrap().props.prop("contents/gfill/colors").unwrap().uid;
        let before = s.active_comp().unwrap().layers.last().unwrap().props.find(prop).unwrap().value.clone();
        let g = Gradient { colors: vec![(0.0, [0.2, 0.4, 0.8, 0.6]), (1.0, [1.0; 4])], color_midpoints: vec![0.2], ..Default::default() };
        let value = Value::Gradient(Box::new(g));
        s.execute("prop.set", json!({"layer": layer, "prop": prop, "value": value})).unwrap();
        assert_eq!(s.active_comp().unwrap().layers.last().unwrap().props.find(prop).unwrap().value, value);
        s.execute("edit.undo", json!({})).unwrap();
        assert_eq!(s.active_comp().unwrap().layers.last().unwrap().props.find(prop).unwrap().value, before);
    }
}
