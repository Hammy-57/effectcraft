//! Motion Library ▸ Tools (the rail's Tools button): the web panel's EffectCraft toolkit in the
//! native panel — Workflow, Text Split, Bounce and Colour (`toolkit_*` operations of the Motion
//! Library's EffectCraft plug, run on the open composition).

use egui::Rect;
use serde_json::{Value, json};

use super::motion_library::ViewState;
use crate::EffectcraftApp;

/// The Tools workspace in `rect` (right of the rail).
pub fn workspace(app: &mut EffectcraftApp, ui: &mut egui::Ui, ctx: &egui::Context, rect: Rect, view: &mut ViewState, s: f32) {
    let _ = (app, ui, ctx, rect, view, s);
}

/// `motionLibrary.tool`: run one Motion Library tool.
pub fn command(app: &mut EffectcraftApp, ctx: &egui::Context, params: &Value) -> Result<Value, String> {
    let _ = (app, ctx, params);
    Ok(json!({}))
}
