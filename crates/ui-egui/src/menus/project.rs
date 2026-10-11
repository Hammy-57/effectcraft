//! Project panel commands.

use serde_json::{Value, json};

use super::dispatch::Handler;
use crate::EffectcraftApp;

pub(crate) fn handlers() -> &'static [Handler] {
    const H: &[Handler] = &[
        Handler::ui("project.column", column),
        Handler::ui("project.deleteWithoutConfirmation", delete_without_confirmation),
        // Run by id (control channel, MCP): the Project panel's selection, whatever has focus.
        Handler::frontend("project.deleteWithoutConfirmation", delete_confirmed),
    ];
    H
}

/// Shift+Delete deletes the Project panel's selection without asking; it does nothing in the
/// other panels.
fn delete_without_confirmation(app: &mut EffectcraftApp, ctx: &egui::Context, _id: &str, _params: Value) -> Result<Value, String> {
    if app.ui.focused != crate::dock::PanelKind::Project {
        return Ok(Value::Null);
    }
    crate::panels::delete_items::delete_confirmed(app, ctx, json!({}))
}

fn delete_confirmed(app: &mut EffectcraftApp, ctx: &egui::Context, _id: &str, _params: Value) -> Result<Value, String> {
    crate::panels::delete_items::delete_confirmed(app, ctx, json!({}))
}

fn column(app: &mut EffectcraftApp, _ctx: &egui::Context, _id: &str, params: Value) -> Result<Value, String> {
    use crate::panels::project::{COLUMNS, set_column};
    let col = params.get("column").and_then(Value::as_str).ok_or("project.column: need `column`")?;
    if !COLUMNS.iter().any(|c| c.0 == col) {
        return Err(format!("unknown column `{col}` (one of: {})", COLUMNS.map(|c| c.0).join(", ")));
    }
    let on = params.get("visible").and_then(Value::as_bool).unwrap_or(!app.ui.project_columns.iter().any(|c| c == col));
    set_column(&mut app.ui.project_columns, col, on);
    Ok(json!({"columns": app.ui.project_columns}))
}
