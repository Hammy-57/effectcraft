//! Motion Library panel view settings (`motionLibrary.view`): the folder and search shown, the
//! interface scale and card size, open folders, the Display settings popover.

use serde_json::Value;

use super::dispatch::Handler;
use crate::EffectcraftApp;

pub(crate) fn handlers() -> &'static [Handler] {
    const H: &[Handler] = &[Handler::ui("motionLibrary.view", view)];
    H
}

fn view(app: &mut EffectcraftApp, _ctx: &egui::Context, _id: &str, params: Value) -> Result<Value, String> {
    crate::panels::motion_library::set_view(&mut app.ui.motion_library, &params)
}
