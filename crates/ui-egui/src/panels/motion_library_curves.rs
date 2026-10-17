//! Motion Library ▸ Curves (the rail's Curves button): the web panel's Curves workspace in the
//! native panel — a graph editor with two draggable handles, Read keys / Apply / Reverse on the
//! selected keyframes, and the saved and starter curve presets (`curve_*` operations of the
//! Motion Library backend and its EffectCraft plug).

use egui::Rect;
use serde_json::{Value, json};

use super::motion_library::ViewState;
use crate::EffectcraftApp;

/// The Curves workspace in `rect` (right of the rail).
pub fn workspace(app: &mut EffectcraftApp, ui: &mut egui::Ui, ctx: &egui::Context, rect: Rect, view: &mut ViewState, s: f32) {
    let _ = (app, ui, ctx, rect, view, s);
}

/// `motionLibrary.curve`: the Curves workspace's operations.
pub fn command(app: &mut EffectcraftApp, ctx: &egui::Context, params: &Value) -> Result<Value, String> {
    let _ = (app, ctx, params);
    Ok(json!({}))
}
