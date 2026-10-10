//! The one-time Figma pairing prompt: the app's Figma bridge asks (control method
//! `ui.figmaPair {client}`) whether the EffectCraft Transfer plug-in may send layers. Allow hands
//! the plug-in the pairing code, which it keeps; Deny refuses it. The reply goes out when the
//! person answers, or as a denial when the prompt times out.
//!
//! Automation ids: `figmaPair.allow`, `figmaPair.deny`.

use egui::{Color32, RichText, vec2};
use serde_json::{Value, json};

use crate::EffectcraftApp;
use crate::i18n::tr;

/// How long the prompt waits for an answer (seconds).
pub const TIMEOUT_S: f64 = 120.0;

/// A pending pairing request.
pub struct Prompt {
    /// Who asks (from the plug-in, shown as given).
    pub client: String,
    pub reply: std::sync::mpsc::Sender<Value>,
    /// UI time after which it counts as denied.
    pub until: f64,
}

/// `ui.figmaPair {client}`: show the prompt and keep the reply for the answer. `false` when a
/// prompt is already open (the caller replies with an error).
pub fn ask(app: &mut EffectcraftApp, ctx: &egui::Context, client: &str, reply: std::sync::mpsc::Sender<Value>) -> bool {
    if app.figma_pair.is_some() {
        return false;
    }
    let client: String = client.chars().filter(|c| !c.is_control()).take(60).collect();
    let now = ctx.input(|i| i.time);
    app.figma_pair = Some(Prompt { client, reply, until: now + TIMEOUT_S });
    // EffectCraft is usually behind Figma: bring it forward so the prompt is seen.
    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
    ctx.send_viewport_cmd(egui::ViewportCommand::RequestUserAttention(egui::UserAttentionType::Informational));
    ctx.request_repaint();
    true
}

fn answer(app: &mut EffectcraftApp, allowed: bool) {
    if let Some(p) = app.figma_pair.take() {
        let _ = p.reply.send(json!({"ok": true, "result": {"allowed": allowed}}));
        app.ui.status =
            if allowed { tr("Figma connected: the EffectCraft Transfer plug-in can send layers") } else { tr("Figma connection denied") }.to_string();
    }
}

pub fn show(app: &mut EffectcraftApp, ctx: &egui::Context) {
    let Some(p) = &app.figma_pair else { return };
    let now = ctx.input(|i| i.time);
    if now > p.until {
        answer(app, false);
        return;
    }
    let client = p.client.clone();
    ctx.request_repaint_after(std::time::Duration::from_secs(1));
    let t = app.tokens;
    let mut choice = None;
    super::dialogs::modal(ctx, tr("Connect Figma?"), vec2(460.0, 200.0), &t, |ui| {
        ui.label(tr("The EffectCraft Transfer plug-in in Figma wants to send layers to EffectCraft."));
        ui.add_space(4.0);
        ui.label(RichText::new(format!("{}: {client}", tr("Asked by"))).color(t.text_dim));
        ui.label(RichText::new(tr("Allow only if you just clicked Connect in the plug-in. You're asked once; the plug-in remembers it.")).color(t.text_dim));
        ui.add_space(14.0);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let b = ui.add(egui::Button::new(RichText::new(tr(" Allow ")).color(Color32::WHITE)).fill(t.accent));
            app.auto.add("figmaPair.allow", b.rect, "Allow");
            if b.clicked() {
                choice = Some(true);
            }
            let b = ui.button(tr("Deny"));
            app.auto.add("figmaPair.deny", b.rect, "Deny");
            if b.clicked() {
                choice = Some(false);
            }
        });
    });
    if let Some(c) = choice {
        answer(app, c);
    }
}
