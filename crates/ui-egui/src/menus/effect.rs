//! Effect and animation-preset commands with frontend parts.

use serde_json::{Value, json};

use super::dispatch::Handler;
use crate::EffectcraftApp;
use crate::dock::PanelKind;

pub(crate) fn handlers() -> &'static [Handler] {
    const H: &[Handler] = &[
        Handler::frontend("effect.manage", effects_presets),
        Handler::frontend("anim.browsePresets", effects_presets),
        Handler::frontend("effects.copyPresetsFolderPath", copy_presets_folder_path),
        Handler::frontend("mediaBrowser.copySourcePath", copy_source_path),
    ];
    H
}

fn copy_source_path(_app: &mut EffectcraftApp, ctx: &egui::Context, _id: &str, p: Value) -> Result<Value, String> {
    let path = p.get("path").and_then(Value::as_str).filter(|path| !path.is_empty()).ok_or("A source path is required")?;
    ctx.copy_text(path.to_string());
    Ok(json!({"path": path}))
}

fn copy_presets_folder_path(_app: &mut EffectcraftApp, ctx: &egui::Context, _id: &str, _p: Value) -> Result<Value, String> {
    if cfg!(target_arch = "wasm32") {
        return Err("The web app has no local presets folder".into());
    }
    let path = crate::panels::effects_presets::presets_dir().filter(|path| !path.as_os_str().is_empty()).ok_or("No presets folder is configured")?;
    let path = std::path::absolute(path).map_err(|e| format!("Cannot resolve the presets folder path: {e}"))?;
    let path = path.to_str().ok_or("The presets folder path is not valid UTF-8")?;
    ctx.copy_text(path.to_string());
    Ok(json!({"path": path}))
}

/// The Effects & Presets panel.
fn effects_presets(app: &mut EffectcraftApp, _ctx: &egui::Context, _id: &str, _p: Value) -> Result<Value, String> {
    app.show_panel(PanelKind::EffectsPresets);
    Ok(Value::Null)
}
