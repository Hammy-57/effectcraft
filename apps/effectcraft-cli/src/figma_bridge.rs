//! `effectcraft-cli figma-bridge`: the Figma plug-in's local HTTP endpoint, standalone. It
//! forwards each transfer to the running app's control channel (`file.importFigma
//! {payloadPath}`, one undo step). The desktop app runs the same bridge itself at launch
//! (Settings ▸ Scripting & Expressions ▸ Figma Bridge, with one-time pairing); this command is
//! for when that is off, or for a headless setup. See `effectcraft_automation::figma_bridge`.

use std::sync::Arc;

use effectcraft_automation::BridgeClient;
use effectcraft_automation::figma_bridge::{self, Bridge, ControlHost};

pub struct Config {
    pub port: u16,
    pub control: String,
    pub code: Option<String>,
}

pub fn serve(cfg: Config) -> Result<(), String> {
    let listener = figma_bridge::bind(cfg.port)?;
    // Validate the control address up front (loopback only).
    BridgeClient::new(&cfg.control).map_err(|e| e.to_string())?;
    let code = figma_bridge::pairing_code(effectcraft_host::config_dir().as_deref(), cfg.code.as_deref())?;
    eprintln!("EffectCraft Figma bridge on http://127.0.0.1:{} → EffectCraft control channel {}", cfg.port, cfg.control);
    eprintln!("Pairing code (paste it into the Figma plug-in once): {code}");
    let bridge = Arc::new(Bridge::new(Arc::new(ControlHost { control: cfg.control }), code, cfg.port));
    bridge.serve(listener);
    Ok(())
}
