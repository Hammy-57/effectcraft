//! File menu commands with frontend parts: Close (the focused panel), Interpret Footage, New
//! Project from Template.

use serde_json::{Value, json};

use super::dispatch::Handler;
use super::no_params;
use crate::EffectcraftApp;
use crate::dock::PanelKind;

pub(crate) fn handlers() -> &'static [Handler] {
    const H: &[Handler] = &[
        Handler::ui("file.close", close),
        Handler::frontend("file.interpretFootage", interpret_footage),
        // File ▸ New ▸ New Project from Template…: the Home screen's Templates tab.
        Handler::frontend("file.newFromTemplate", new_from_template),
    ];
    H
}

/// File ▸ Close (Cmd+W) closes the focused panel, as in After Effects; on a Composition or
/// Timeline panel (or with a `comp`), its comp (the engine command).
fn close(app: &mut EffectcraftApp, ctx: &egui::Context, id: &str, params: Value) -> Result<Value, String> {
    if no_params(&params) {
        let p = app.ui.focused;
        if !matches!(p, PanelKind::Composition | PanelKind::Viewer(_) | PanelKind::Timeline) && app.panel_group(p).is_some() {
            app.close_panel(p);
            return Ok(json!({"closed": p.id()}));
        }
    }
    super::engine(app, ctx, id, params)
}

fn interpret_footage(app: &mut EffectcraftApp, _ctx: &egui::Context, _id: &str, p: Value) -> Result<Value, String> {
    // Import ▸ Interpret Unlabeled Alpha As: Ask User.
    if let Some(i) = p.get("item").and_then(Value::as_u64) {
        app.session.state.project_selection = vec![effectcraft_engine::project::ItemId(i)];
    }
    crate::panels::dialogs::open_form(app, "file.interpretFootage", &json!({}));
    Ok(Value::Null)
}

fn new_from_template(app: &mut EffectcraftApp, _ctx: &egui::Context, _id: &str, _p: Value) -> Result<Value, String> {
    app.ui.start_screen = true;
    app.ui.home_learn = false;
    app.ui.home_templates = true;
    Ok(Value::Null)
}
