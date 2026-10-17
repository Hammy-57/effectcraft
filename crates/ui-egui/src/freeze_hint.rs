//! The Timeline's hint for a layer too heavy to play in real time ("Stat Stack renders at
//! 1.8 s/frame — Freeze it?"), and the feed of the viewer's per-layer render times that finds
//! it (the engine's `freeze.suggest`). Freezing always goes through `freeze.freeze`; with
//! `previews.freezeAutoHeavy` off nothing is frozen until the button is pressed.

use effectcraft_engine::project::ItemId;
use egui::{Align2, Rect, Stroke, pos2, vec2};
use serde_json::json;

use crate::{EffectcraftApp, frames::FrameKey, panels::panel_kit as kit, theme::Tokens};

/// Hand the engine the layer times of the frame the viewer shows, once per frame.
pub fn feed(app: &mut EffectcraftApp, ctx: &egui::Context) {
    let Some(key) = app.viewer_shown.as_ref().map(|(_, key)| *key) else {
        return;
    };
    let id = egui::Id::new("freeze-hint-fed");
    if ctx.data(|d| d.get_temp::<FrameKey>(id)) == Some(key) {
        return;
    }
    ctx.data_mut(|d| d.insert_temp(id, key));
    let Some(times) = app.frames.times.get(&key).filter(|t| !t.remote) else {
        return;
    };
    let costs: Vec<_> = times.layers.iter().map(|(&(comp, layer), &ms)| (comp, layer, ms)).collect();
    app.session.note_layer_costs(ItemId(key.comp), costs);
}

/// The hint, in `area` (the Timeline's graph above its footer), while a layer stays far above
/// the frame budget and nothing is being frozen.
pub fn show(app: &mut EffectcraftApp, ui: &mut egui::Ui, area: Rect) {
    if cfg!(target_arch = "wasm32") || app.session.jobs().iter().any(|j| j.kind == "smartFreeze") {
        return;
    }
    let Some(top) = app.session.freeze_suggestions().into_iter().next() else {
        return;
    };
    let t = app.tokens;
    let text = format!("{} — Freeze it?", top.hint());
    let font = Tokens::ui(11.5);
    let width = ui.painter().layout_no_wrap(text.clone(), font.clone(), t.text).size().x;
    let bar = Rect::from_min_size(pos2(area.min.x + 10.0, area.max.y - 30.0), vec2((width + 140.0).min(area.width() - 20.0), 24.0));
    if bar.width() < 120.0 {
        return;
    }
    let p = ui.painter();
    p.rect_filled(bar, 6.0, t.panel_bg);
    p.rect_stroke(bar, 6.0, Stroke::new(1.0, t.accent), egui::StrokeKind::Inside);
    p.text(pos2(bar.min.x + 10.0, bar.center().y), Align2::LEFT_CENTER, &text, font, t.text);
    app.auto.add("timeline.freezeHint", bar, &text);
    let freeze = Rect::from_min_size(pos2(bar.max.x - 90.0, bar.min.y + 2.0), vec2(60.0, 20.0));
    if kit::button(app, ui, freeze, "timeline.freezeHint.freeze", "Freeze", true) {
        kit::exec(app, "freeze.freeze", top.freeze_params());
    }
    let dismiss = Rect::from_min_size(pos2(bar.max.x - 26.0, bar.min.y + 2.0), vec2(20.0, 20.0));
    if kit::button(app, ui, dismiss, "timeline.freezeHint.dismiss", "×", false) {
        kit::exec(app, "freeze.suggest", json!({"dismiss": {"comp": top.comp.0, "layer": top.layer.0}}));
    }
}
