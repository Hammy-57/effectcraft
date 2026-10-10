//! Application commands: Home, the new-composition / settings / solid dialogs, About, Settings,
//! the command palette, Hide / Quit and other application windows.

use serde_json::{Value, json};

use super::dispatch::Handler;
use super::no_params;
use crate::EffectcraftApp;
use crate::dock::PanelKind;

pub(crate) fn handlers() -> &'static [Handler] {
    const H: &[Handler] = &[
        Handler::ui("app.home", home),
        // Without params these open their dialogs; with params they go to the engine.
        Handler::ui("app.newComp", new_comp),
        Handler::ui("comp.new", new_comp),
        Handler::ui("app.compSettings", comp_settings),
        Handler::ui("comp.settings", comp_settings),
        Handler::ui("app.solidSettings", solid_settings),
        Handler::ui("layer.newSolid", solid_settings),
        Handler::frontend("app.about", about),
        Handler::frontend("app.settings", settings),
        Handler::frontend("app.gpuInfo", gpu_info),
        Handler::frontend("app.hide", hide),
        Handler::frontend("app.hideOthers", hide),
        Handler::frontend("app.showAll", hide),
        Handler::frontend("app.quit", quit),
        Handler::frontend("app.commandPalette", command_palette),
        Handler::frontend("app.fxConsole", fx_console),
        Handler::frontend("app.keyboardShortcuts", keyboard_shortcuts),
        Handler::frontend("app.templates", templates),
        Handler::frontend("app.find", find),
        Handler::frontend("app.confirmExecute", confirm_execute),
        Handler::frontend("app.showReport", show_report),
    ];
    H
}

fn home(app: &mut EffectcraftApp, _ctx: &egui::Context, _id: &str, _params: Value) -> Result<Value, String> {
    app.ui.start_screen = !app.ui.start_screen;
    Ok(Value::Null)
}

fn new_comp(app: &mut EffectcraftApp, ctx: &egui::Context, id: &str, params: Value) -> Result<Value, String> {
    if !no_params(&params) {
        return super::engine(app, ctx, id, params);
    }
    crate::panels::dialogs::open_new_comp(app);
    Ok(Value::Null)
}

fn comp_settings(app: &mut EffectcraftApp, ctx: &egui::Context, id: &str, params: Value) -> Result<Value, String> {
    if !no_params(&params) {
        return super::engine(app, ctx, id, params);
    }
    crate::panels::dialogs::open_comp_settings(app)?;
    Ok(Value::Null)
}

fn solid_settings(app: &mut EffectcraftApp, ctx: &egui::Context, id: &str, params: Value) -> Result<Value, String> {
    if !no_params(&params) {
        return super::engine(app, ctx, id, params);
    }
    crate::panels::dialogs::open_new_solid(app)?;
    Ok(Value::Null)
}

fn about(app: &mut EffectcraftApp, _ctx: &egui::Context, _id: &str, _p: Value) -> Result<Value, String> {
    app.dialog = Some(crate::Dialog::About);
    Ok(Value::Null)
}

fn settings(app: &mut EffectcraftApp, _ctx: &egui::Context, _id: &str, p: Value) -> Result<Value, String> {
    let page = p.get("page").and_then(Value::as_str).unwrap_or("general");
    crate::panels::settings::open(app, effectcraft_engine::prefs::page_id(page).unwrap_or("general"));
    Ok(Value::Null)
}

fn gpu_info(app: &mut EffectcraftApp, _ctx: &egui::Context, _id: &str, _p: Value) -> Result<Value, String> {
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
    crate::panels::dialogs::info(
        app,
        "GPU Information",
        &format!(
            "Compositing: CPU, {threads} threads (pure Rust, rayon).\nDisplay: egui on wgpu.\nLayer cache: {} MB  •  Preview cache: {} MB.",
            app.session.layer_cache.budget() >> 20,
            app.frames.budget() >> 20
        ),
    );
    Ok(Value::Null)
}

fn hide(app: &mut EffectcraftApp, ctx: &egui::Context, id: &str, _p: Value) -> Result<Value, String> {
    // The desktop app asks macOS (NSApplication hide / hideOtherApplications /
    // unhideAllApplications); elsewhere Hide minimizes and the others don't apply.
    if app.hooks.app_action.as_ref().is_some_and(|f| f(id)) {
        return Ok(Value::Null);
    }
    match id {
        "app.hide" => ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true)),
        _ => return Err(format!("`{id}` is only available on macOS")),
    }
    Ok(Value::Null)
}

fn quit(_app: &mut EffectcraftApp, ctx: &egui::Context, _id: &str, _p: Value) -> Result<Value, String> {
    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    Ok(Value::Null)
}

fn command_palette(app: &mut EffectcraftApp, _ctx: &egui::Context, _id: &str, p: Value) -> Result<Value, String> {
    app.dialog_state.palette_query = p.get("query").and_then(Value::as_str).unwrap_or_default().to_string();
    app.dialog_state.palette_sel = 0;
    app.dialog = Some(crate::Dialog::CommandPalette);
    Ok(Value::Null)
}

fn fx_console(app: &mut EffectcraftApp, ctx: &egui::Context, _id: &str, p: Value) -> Result<Value, String> {
    crate::panels::fx_console::open(app, ctx, p.get("query").and_then(Value::as_str).unwrap_or_default());
    Ok(Value::Null)
}

fn keyboard_shortcuts(app: &mut EffectcraftApp, _ctx: &egui::Context, _id: &str, _p: Value) -> Result<Value, String> {
    crate::panels::shortcut_editor::open(app);
    Ok(Value::Null)
}

fn templates(app: &mut EffectcraftApp, ctx: &egui::Context, _id: &str, p: Value) -> Result<Value, String> {
    let kind = p.get("kind").and_then(Value::as_str).unwrap_or("renderSettings");
    crate::panels::rq_templates::open(app, ctx, kind);
    Ok(Value::Null)
}

fn find(app: &mut EffectcraftApp, _ctx: &egui::Context, _id: &str, p: Value) -> Result<Value, String> {
    if let Some(q) = p.get("query").and_then(Value::as_str) {
        app.ui.project_search = q.to_string();
    }
    app.show_panel(PanelKind::Project);
    Ok(Value::Null)
}

fn confirm_execute(app: &mut EffectcraftApp, _ctx: &egui::Context, _id: &str, p: Value) -> Result<Value, String> {
    // Settings ▸ Scripting ▸ Warn User When Executing Files.
    let path = p.get("path").and_then(Value::as_str).unwrap_or_default().to_string();
    crate::panels::forms::form(
        app,
        "A script wants to open a file",
        "file.executeFile",
        json!({"path": path, "confirmed": true}),
        vec![crate::panels::forms::Field::text("path", "Open", &path)],
    );
    Ok(Value::Null)
}

fn show_report(app: &mut EffectcraftApp, _ctx: &egui::Context, _id: &str, p: Value) -> Result<Value, String> {
    let title = p.get("title").and_then(Value::as_str).unwrap_or("Report").to_string();
    let text = p.get("text").and_then(Value::as_str).unwrap_or_default().to_string();
    crate::panels::dialogs::info(app, &title, &text);
    Ok(Value::Null)
}
