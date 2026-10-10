//! Flowchart panel options and graph commands (handled by `panels::flowchart`).

use serde_json::Value;

use super::dispatch::Handler;
use crate::EffectcraftApp;

pub(crate) fn handlers() -> &'static [Handler] {
    const H: &[Handler] = &[Handler::ui("flowchart.options", command), Handler::ui("flowchart.graph", command)];
    H
}

fn command(app: &mut EffectcraftApp, _ctx: &egui::Context, id: &str, params: Value) -> Result<Value, String> {
    crate::panels::flowchart::command(app, id, &params)
}
