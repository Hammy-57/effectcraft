//! Motion Library panel view settings (`motionLibrary.view`): the folder and search shown, the
//! interface scale and card size, open folders, filters, popovers and details; `motionLibrary.apply`
//! (a card dropped on the Timeline or the viewer); `motionLibrary.copyName` and `motionLibrary.save` (a card's menu).

use serde_json::Value;

use super::dispatch::Handler;
use crate::EffectcraftApp;

pub(crate) fn handlers() -> &'static [Handler] {
    const H: &[Handler] = &[
        Handler::ui("motionLibrary.view", view),
        Handler::ui("motionLibrary.apply", apply),
        Handler::ui("motionLibrary.copyName", copy_name),
        Handler::ui("motionLibrary.save", save),
    ];
    H
}

fn view(app: &mut EffectcraftApp, _ctx: &egui::Context, _id: &str, params: Value) -> Result<Value, String> {
    crate::panels::motion_library::set_view(&mut app.ui.motion_library, &params)
}

fn apply(app: &mut EffectcraftApp, _ctx: &egui::Context, _id: &str, params: Value) -> Result<Value, String> {
    crate::panels::motion_library::start_apply(&mut app.ui.motion_library, &params)
}

fn copy_name(app: &mut EffectcraftApp, ctx: &egui::Context, _id: &str, params: Value) -> Result<Value, String> {
    let id = params.get("id").and_then(Value::as_str).ok_or("missing `id` (a Motion Library card id)")?;
    let cat = app.session.motion_library()?.catalog().ok_or("the Motion Library catalog isn't loaded yet")?;
    let name = cat.card(id).map(|c| c.name.clone()).ok_or(format!("no Motion Library card `{id}`"))?;
    ctx.copy_text(name.clone());
    Ok(serde_json::json!({"copied": name}))
}

fn save(app: &mut EffectcraftApp, _ctx: &egui::Context, _id: &str, params: Value) -> Result<Value, String> {
    crate::panels::motion_library::start_save(app, &params)
}
