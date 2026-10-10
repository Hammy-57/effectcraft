//! Composition commands with frontend parts: the Flowchart and the Mini-Flowchart.

use serde_json::Value;

use super::dispatch::Handler;
use crate::EffectcraftApp;
use crate::dock::PanelKind;

pub(crate) fn handlers() -> &'static [Handler] {
    const H: &[Handler] = &[Handler::frontend("comp.flowchart", flowchart), Handler::frontend("comp.miniFlowchart", mini_flowchart)];
    H
}

fn flowchart(app: &mut EffectcraftApp, _ctx: &egui::Context, _id: &str, _p: Value) -> Result<Value, String> {
    // Chart the active comp; Layer ▸ Reveal ▸ Reveal Layer in Project Flowchart shows
    // layers and selects the layer's node.
    app.ui.flowchart.root = app.session.active_comp_id().map(|c| c.0);
    if let (Some(c), Some(l)) = (app.session.active_comp_id(), app.session.state.selected_layers.first()) {
        app.ui.flowchart.layers = true;
        app.ui.flowchart.selected = Some(format!("layer:{}:{}", c.0, l.0));
    }
    app.show_panel(PanelKind::Flowchart);
    Ok(Value::Null)
}

fn mini_flowchart(app: &mut EffectcraftApp, ctx: &egui::Context, _id: &str, _p: Value) -> Result<Value, String> {
    let at = ctx.input(|i| i.pointer.hover_pos()).unwrap_or_else(|| ctx.content_rect().center());
    app.ui.mini_flowchart = Some([at.x, at.y]);
    Ok(Value::Null)
}
