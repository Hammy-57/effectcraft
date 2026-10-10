//! Help menu commands with frontend parts.

use serde_json::Value;

use super::dispatch::Handler;
use crate::EffectcraftApp;

pub(crate) fn handlers() -> &'static [Handler] {
    // Help ▸ In-App Tutorials: the Home screen's Learn tab.
    const H: &[Handler] = &[Handler::frontend("help.inAppTutorials", in_app_tutorials)];
    H
}

fn in_app_tutorials(app: &mut EffectcraftApp, _ctx: &egui::Context, _id: &str, _p: Value) -> Result<Value, String> {
    app.ui.start_screen = true;
    app.ui.home_learn = true;
    app.ui.home_templates = false;
    Ok(Value::Null)
}
