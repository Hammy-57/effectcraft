//! Tools panel shortcuts (`tool.<name>`): pick a tool; a slot shortcut cycles through its tools.

use serde_json::{Value, json};

use super::dispatch::Handler;
use crate::EffectcraftApp;
use crate::state::Tool;

pub(crate) fn handlers() -> &'static [Handler] {
    const H: &[Handler] = &[Handler::ui_prefix("tool.", tool)];
    H
}

fn tool(app: &mut EffectcraftApp, _ctx: &egui::Context, id: &str, _params: Value) -> Result<Value, String> {
    let t = id.strip_prefix("tool.").unwrap_or(id);
    // Slot shortcuts cycle through the slot's tools.
    let slot = match t {
        "shape" => Some(8),
        "pen" => Some(9),
        "type" => Some(10),
        "brush" => Some(11),
        "rotoBrush" => Some(14),
        "puppet" => Some(15),
        _ => None,
    };
    if let Some(si) = slot {
        let tools: Vec<Tool> = match si {
            11 => vec![Tool::Brush, Tool::Clone, Tool::Eraser],
            _ => crate::state::Tool::SLOTS[si].to_vec(),
        };
        let cur = tools.iter().position(|x| *x == app.ui.tool);
        let next = match cur {
            Some(i) => tools[(i + 1) % tools.len()],
            None => app.ui.slot_tools.get(si).copied().filter(|x| tools.contains(x)).unwrap_or(tools[0]),
        };
        app.ui.tool = next;
        if si < app.ui.slot_tools.len() && si != 11 {
            app.ui.slot_tools[si] = next;
        }
        return Ok(json!({"tool": next}));
    }
    let tool = Tool::from_name(t).ok_or_else(|| format!("unknown tool `{t}`"))?;
    app.ui.tool = tool;
    Ok(json!({"tool": tool}))
}
