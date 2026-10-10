//! Project panel commands.

use serde_json::{Value, json};

use super::dispatch::Handler;
use crate::EffectcraftApp;

pub(crate) fn handlers() -> &'static [Handler] {
    const H: &[Handler] = &[Handler::ui("project.column", column)];
    H
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
