//! Tracker panel dialogs.

use serde_json::Value;

use super::dispatch::Handler;
use crate::EffectcraftApp;

pub(crate) fn handlers() -> &'static [Handler] {
    const H: &[Handler] = &[Handler::frontend("track.editTargetDialog", edit_target), Handler::frontend("track.optionsDialog", options)];
    H
}

fn edit_target(app: &mut EffectcraftApp, _ctx: &egui::Context, _id: &str, _p: Value) -> Result<Value, String> {
    crate::panels::tracker::open_target(app)?;
    Ok(Value::Null)
}

fn options(app: &mut EffectcraftApp, _ctx: &egui::Context, _id: &str, _p: Value) -> Result<Value, String> {
    crate::panels::tracker::open_options(app)?;
    Ok(Value::Null)
}
