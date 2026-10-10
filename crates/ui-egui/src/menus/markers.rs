//! Marker commands.

use serde_json::{Value, json};

use super::dispatch::Handler;
use crate::EffectcraftApp;

pub(crate) fn handlers() -> &'static [Handler] {
    const H: &[Handler] = &[Handler::ui("markers.dialog", dialog)];
    H
}

/// The Composition/Layer Marker dialog (double-click a marker): {layer?, index}.
fn dialog(app: &mut EffectcraftApp, _ctx: &egui::Context, id: &str, params: Value) -> Result<Value, String> {
    let index = params.get("index").and_then(Value::as_u64).ok_or("markers.dialog: need `index`")? as usize;
    let layer = params.get("layer").and_then(Value::as_u64);
    crate::panels::markers_ui::open_dialog(app, crate::panels::markers_ui::MarkerRef { layer, index })?;
    Ok(json!({"dialog": id}))
}
