//! Render Queue frontend events.

use serde_json::Value;

use super::dispatch::Handler;
use crate::EffectcraftApp;

pub(crate) fn handlers() -> &'static [Handler] {
    const H: &[Handler] = &[Handler::frontend("renderQueue.notify", notify)];
    H
}

fn notify(app: &mut EffectcraftApp, ctx: &egui::Context, _id: &str, p: Value) -> Result<Value, String> {
    let now = ctx.input(|i| i.time);
    // Render finished with Notify on: a toast, the dock/taskbar attention request, and the
    // host's system sound (the desktop app handles `renderQueue.notify`).
    let msg = p.get("message").and_then(Value::as_str).unwrap_or("Render finished").to_string();
    app.toast = Some((msg, now));
    ctx.send_viewport_cmd(egui::ViewportCommand::RequestUserAttention(egui::UserAttentionType::Informational));
    if let Some(f) = app.hooks.app_action.as_ref() {
        f("renderQueue.notify");
    }
    Ok(Value::Null)
}
