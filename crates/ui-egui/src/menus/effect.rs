//! Effect and animation-preset commands with frontend parts.

use serde_json::Value;

use super::dispatch::Handler;
use crate::EffectcraftApp;
use crate::dock::PanelKind;

pub(crate) fn handlers() -> &'static [Handler] {
    const H: &[Handler] = &[Handler::frontend("effect.manage", effects_presets), Handler::frontend("anim.browsePresets", effects_presets)];
    H
}

/// The Effects & Presets panel.
fn effects_presets(app: &mut EffectcraftApp, _ctx: &egui::Context, _id: &str, _p: Value) -> Result<Value, String> {
    app.show_panel(PanelKind::EffectsPresets);
    Ok(Value::Null)
}
