//! Keyframe dialogs: Keyframe Velocity and Keyframe Interpolation open without params; with
//! params the command goes to the engine.

use serde_json::Value;

use super::dispatch::Handler;
use super::no_params;
use crate::EffectcraftApp;

pub(crate) fn handlers() -> &'static [Handler] {
    const H: &[Handler] = &[Handler::ui("keys.velocity", velocity), Handler::ui("keys.interpolation", interpolation)];
    H
}

fn velocity(app: &mut EffectcraftApp, ctx: &egui::Context, id: &str, params: Value) -> Result<Value, String> {
    if !no_params(&params) {
        return super::engine(app, ctx, id, params);
    }
    crate::panels::key_dialogs::open_velocity(app)?;
    Ok(Value::Null)
}

fn interpolation(app: &mut EffectcraftApp, ctx: &egui::Context, id: &str, params: Value) -> Result<Value, String> {
    if !no_params(&params) {
        return super::engine(app, ctx, id, params);
    }
    crate::panels::key_dialogs::open_interpolation(app)?;
    Ok(Value::Null)
}
