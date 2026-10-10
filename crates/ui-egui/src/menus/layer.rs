//! Layer commands with frontend parts: the Time Stretch dialog, Layer Styles options, the Layer
//! panel.

use serde_json::Value;

use super::dispatch::Handler;
use super::no_params;
use crate::EffectcraftApp;
use crate::dock::PanelKind;

pub(crate) fn handlers() -> &'static [Handler] {
    const H: &[Handler] = &[
        Handler::ui("layer.timeStretch", time_stretch),
        Handler::frontend("layer.style.options", style_options),
        Handler::frontend("layer.openLayer", open_layer),
    ];
    H
}

/// Without params: the Time Stretch dialog; with params: the engine.
fn time_stretch(app: &mut EffectcraftApp, ctx: &egui::Context, id: &str, params: Value) -> Result<Value, String> {
    if !no_params(&params) {
        return super::engine(app, ctx, id, params);
    }
    crate::panels::key_dialogs::open_time_stretch(app)?;
    Ok(Value::Null)
}

fn style_options(app: &mut EffectcraftApp, _ctx: &egui::Context, _id: &str, p: Value) -> Result<Value, String> {
    crate::panels::layer_styles_dialog::open(app, &p)?;
    Ok(Value::Null)
}

fn open_layer(app: &mut EffectcraftApp, _ctx: &egui::Context, _id: &str, p: Value) -> Result<Value, String> {
    let id = p.get("layer").and_then(Value::as_u64).or_else(|| app.session.state.selected_layers.first().map(|l| l.0));
    app.ui.layer_panel = id;
    app.ui.layer_view = None;
    app.show_panel(PanelKind::Layer);
    Ok(Value::Null)
}
