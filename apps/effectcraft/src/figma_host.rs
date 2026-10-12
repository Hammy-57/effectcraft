//! The Figma bridge, hosted by the app (Settings ▸ Scripting & Expressions ▸ Let Figma Send
//! Layers, `scripting.figmaBridge`): the EffectCraft Transfer plug-in posts to
//! `127.0.0.1:7789` and each transfer runs `file.importFigma {payload}` in this session through
//! the app's control queue (one undo step), without a terminal or the TCP control channel. The
//! first connection asks the person (`ui.figmaPair`, an Allow / Deny prompt); the plug-in keeps
//! the pairing code after that. `EFFECTCRAFT_FIGMA_BRIDGE_PORT` picks another port (tests).

use std::sync::Arc;
use std::sync::mpsc::Sender;
use std::time::Duration;

use effectcraft_automation::figma_bridge::{self, FigmaHost, Pairing};
use effectcraft_ui_egui::ControlRequest;
use serde_json::{Value, json};

/// Long imports (many images) may take a while on the UI thread.
const IMPORT_TIMEOUT: Duration = Duration::from_secs(600);
/// A little longer than the prompt's own timeout.
const PAIR_TIMEOUT: Duration = Duration::from_secs(130);

struct AppHost {
    tx: Sender<ControlRequest>,
    ctx: egui::Context,
}

impl AppHost {
    /// One request through the app's control queue: the `result`, or the error.
    fn call(&self, method: &str, params: Value, timeout: Duration) -> Result<Value, String> {
        let (req, rx) = ControlRequest::new(method, params);
        self.tx.send(req).map_err(|_| "EffectCraft is closing".to_string())?;
        self.ctx.request_repaint();
        let r = rx.recv_timeout(timeout).map_err(|_| "EffectCraft didn't answer in time".to_string())?;
        if r.get("ok").and_then(Value::as_bool) == Some(true) {
            Ok(r.get("result").cloned().unwrap_or(Value::Null))
        } else {
            Err(r.get("error").and_then(Value::as_str).unwrap_or("failed").to_string())
        }
    }
}

impl FigmaHost for AppHost {
    fn name(&self) -> &'static str {
        "app"
    }

    fn status(&self) -> Value {
        // In-process: always reachable while the app runs.
        json!({"connected": true})
    }

    fn import(&self, params: Value) -> Result<Value, String> {
        self.call("engine.execute", json!({"command": "file.importFigma", "params": params}), IMPORT_TIMEOUT)
    }

    fn pair(&self, client: &str) -> Pairing {
        match self.call("ui.figmaPair", json!({"client": client}), PAIR_TIMEOUT) {
            Ok(r) if r.get("allowed").and_then(Value::as_bool) == Some(true) => Pairing::Allowed,
            Ok(_) => Pairing::Denied,
            Err(_) => Pairing::Failed,
        }
    }
}

/// The port: `EFFECTCRAFT_FIGMA_BRIDGE_PORT`, else the plug-in's 7789.
fn port(env: Option<String>) -> u16 {
    env.and_then(|p| p.trim().parse().ok()).unwrap_or(figma_bridge::DEFAULT_PORT)
}

/// Start the bridge on its own thread. A port in use (another EffectCraft, or
/// `effectcraft-cli figma-bridge`) is reported and skipped.
pub fn start(tx: Sender<ControlRequest>, ctx: egui::Context) {
    let port = port(std::env::var("EFFECTCRAFT_FIGMA_BRIDGE_PORT").ok());
    let code = match figma_bridge::pairing_code(effectcraft_host::config_dir().as_deref(), None) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("effectcraft: Figma bridge not started: {e}");
            return;
        }
    };
    match figma_bridge::spawn(Arc::new(AppHost { tx, ctx }), code, port) {
        Ok(p) => eprintln!("effectcraft: Figma bridge listening on 127.0.0.1:{p}"),
        Err(e) => eprintln!("effectcraft: Figma bridge not started: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_from_env() {
        assert_eq!(port(None), 7789);
        assert_eq!(port(Some("7790".into())), 7790);
        assert_eq!(port(Some("junk".into())), 7789);
    }

    #[test]
    fn requests_go_through_the_control_queue() {
        let (tx, rx) = std::sync::mpsc::channel::<ControlRequest>();
        let host = AppHost { tx, ctx: egui::Context::default() };
        // A stand-in for the UI thread: allow the pairing, run the import.
        std::thread::spawn(move || {
            while let Ok(req) = rx.recv() {
                let r = match req.method.as_str() {
                    "ui.figmaPair" => json!({"ok": true, "result": {"allowed": true}}),
                    "engine.execute" => json!({"ok": true, "result": {"command": req.params["command"], "native": 2}}),
                    _ => json!({"ok": false, "error": "unknown"}),
                };
                let _ = req.reply.send(r);
            }
        });
        assert_eq!(host.pair("Figma"), Pairing::Allowed);
        let r = host.import(json!({"payload": {"nodes": []}})).unwrap();
        assert_eq!(r["command"], "file.importFigma");
        assert_eq!(r["native"], 2);
    }
}
