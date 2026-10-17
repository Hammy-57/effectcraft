//! `effectcraft-cli mcp --bridge APP --render-box BOX`: renders and lint go to a render box
//! (`serve --follow APP`, a windowless EffectCraft mirroring the app, see
//! [`effectcraft_engine::remote_follow`]); everything else, edits first, goes to the app. So an
//! agent checking its edits never takes frames from the user's playback, and the app stays the
//! one source of truth.
//!
//! The box pulls the app's latest revision before each render, so a render shows the edit made
//! just before it. When the box is unreachable or out of sync, the render runs in the app as
//! before (said once on stderr, and in the reply's `renderedOn`).

use std::time::{Duration, Instant};

use effectcraft_engine::prefs::Prefs;
use serde_json::Value;

use crate::bridge::{ControlTransport, SessionJob};
use crate::host_server::{OUT_OF_SYNC, STOPPED};
use crate::{Error, Result};

/// Engine commands a render box runs (renders and checks of rendered frames).
pub const BOX_COMMANDS: &[&str] = &["render.frame", "render.contactSheet", "render.lint", "render.snapshot", "render.compare"];

/// After the box failed, renders go to the app for this long before it is tried again.
const RETRY_BOX: Duration = Duration::from_secs(10);

pub struct RenderRoute {
    app: Box<dyn ControlTransport>,
    render: Box<dyn ControlTransport>,
    /// The box failed: renders go to the app until then.
    down_until: Option<Instant>,
    warned: bool,
}

impl RenderRoute {
    pub fn new(app: Box<dyn ControlTransport>, render: Box<dyn ControlTransport>) -> RenderRoute {
        RenderRoute { app, render, down_until: None, warned: false }
    }
}

/// Whether a control request is a render the box should run.
pub fn routed(method: &str, params: &Value) -> bool {
    match method {
        "render.frame" => true,
        "engine.execute" => params.get("command").or(params.get("id")).and_then(Value::as_str).is_some_and(|c| BOX_COMMANDS.contains(&c)),
        _ => false,
    }
}

fn mark(mut v: Value, on: &str) -> Value {
    if let Some(o) = v.as_object_mut() {
        o.insert("renderedOn".into(), Value::from(on));
    }
    v
}

impl ControlTransport for RenderRoute {
    fn call(&mut self, method: &str, params: Value) -> Result<Value> {
        if !routed(method, &params) {
            return self.app.call(method, params);
        }
        if self.down_until.is_none_or(|t| Instant::now() >= t) {
            match self.render.call(method, params.clone()) {
                Ok(v) => {
                    self.down_until = None;
                    return Ok(mark(v, "render-box"));
                }
                // The box answered: its error is the render's (a bad comp id, a bad param).
                Err(Error::App(e)) if !e.starts_with(OUT_OF_SYNC) && !e.starts_with(STOPPED) => return Err(Error::App(e)),
                Err(e) => {
                    if !self.warned {
                        eprintln!("render box unavailable ({e}); rendering in the app until it is back");
                        self.warned = true;
                    }
                    self.down_until = Some(Instant::now() + RETRY_BOX);
                }
            }
        }
        self.app.call(method, params).map(|v| mark(v, "app"))
    }

    fn mode(&self) -> &'static str {
        self.app.mode()
    }

    fn session_prefs(&self) -> Option<Prefs> {
        self.app.session_prefs()
    }

    fn on_session(&mut self, job: SessionJob) -> Option<Result<()>> {
        self.app.on_session(job)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge::BridgeClient;
    use crate::host_server::{HostConfig, READ_ONLY, request, serve};
    use effectcraft_engine::Session;
    use serde_json::json;

    fn host(follow: Option<u16>) -> (u16, std::thread::JoinHandle<std::io::Result<()>>) {
        let (tx, rx) = std::sync::mpsc::channel();
        let config = HostConfig { port: 0, config_dir: None, follow };
        let h = std::thread::spawn(move || serve(config, Box::new(|| Ok(Session::default())), move |p| tx.send(p).unwrap()));
        (rx.recv().unwrap(), h)
    }

    fn exec(t: &mut dyn ControlTransport, command: &str, params: Value) -> Result<Value> {
        t.call("engine.execute", json!({"command": command, "params": params}))
    }

    /// Edits go to the app, renders to the box, which shows the edit made just before; the box
    /// refuses edits of its own; with the box gone renders run in the app.
    #[test]
    fn renders_go_to_the_box_and_see_the_latest_edit() {
        let (app_port, app) = host(None);
        let (box_port, boxed) = host(Some(app_port));
        let mut route =
            RenderRoute::new(Box::new(BridgeClient::new(&app_port.to_string()).unwrap()), Box::new(BridgeClient::new(&box_port.to_string()).unwrap()));
        exec(&mut route, "comp.new", json!({"name": "A", "width": 64, "height": 36, "duration": 4})).unwrap();
        exec(&mut route, "layer.newSolid", json!({"name": "Red", "color": [1, 0, 0, 1], "width": 64, "height": 36})).unwrap();
        let frame = route.call("render.frame", json!({"comp": "A", "base64": true, "max_side": 16})).unwrap();
        assert_eq!(frame["renderedOn"], "render-box", "{frame}");
        let red = frame["png"].clone();
        // An edit, then at once a render of it.
        exec(&mut route, "prop.set", json!({"comp": "A", "layer": "Red", "path": "transform/opacity", "value": 0})).unwrap();
        let frame = exec(&mut route, "render.frame", json!({"comp": "A", "base64": true, "max_side": 16})).unwrap();
        assert_eq!(frame["renderedOn"], "render-box");
        assert_ne!(frame["png"], red, "the box rendered the edit made just before");
        let lint = exec(&mut route, "render.lint", json!({"comp": "A"})).unwrap();
        assert_eq!(lint["renderedOn"], "render-box", "{lint}");
        let app_rev = request(app_port, "app.revision", json!({})).unwrap()["result"]["revision"].clone();
        let info = request(box_port, "host.info", json!({})).unwrap();
        assert_eq!(info["result"]["follow"]["appRevision"], app_rev, "{info}");
        assert_eq!(info["result"]["follow"]["mode"], "sync");
        // Read-only: an edit on the box is undone.
        let mut direct = BridgeClient::new(&box_port.to_string()).unwrap();
        let refused = exec(&mut direct, "layer.newSolid", json!({"comp": "A", "name": "Sneaky"})).unwrap_err();
        assert!(refused.to_string().contains(READ_ONLY), "{refused}");
        let tree = exec(&mut direct, "comp.info", json!({"comp": "A"})).unwrap();
        assert!(!tree.to_string().contains("Sneaky"), "{tree}");
        // The box stops; renders go to the app.
        assert_eq!(request(box_port, "host.release", json!({})).unwrap()["result"]["released"], true);
        boxed.join().unwrap().unwrap();
        let frame = route.call("render.frame", json!({"comp": "A", "base64": true, "max_side": 16})).unwrap();
        assert_eq!(frame["renderedOn"], "app");
        assert_eq!(exec(&mut route, "comp.info", json!({"comp": "A"})).unwrap().get("renderedOn"), None, "reads go to the app as they are");
        request(app_port, "host.release", json!({"path": std::env::temp_dir().join(format!("ec-route-{}.ecproj", std::process::id())).to_string_lossy()}))
            .unwrap();
        app.join().unwrap().unwrap();
    }

    #[test]
    fn only_renders_are_routed() {
        assert!(routed("render.frame", &json!({})));
        assert!(routed("engine.execute", &json!({"command": "render.contactSheet"})));
        assert!(routed("engine.execute", &json!({"command": "render.lint"})));
        assert!(!routed("engine.execute", &json!({"command": "layer.newSolid"})));
        assert!(!routed("engine.execute", &json!({"command": "renderQueue.render"})));
        assert!(!routed("ui.screenshot", &json!({})));
    }
}
