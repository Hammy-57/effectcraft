//! `mcp --ui`: the full app offscreen in this process ([`effectcraft_ui_egui::offscreen`]) as the
//! MCP server's control transport, so every bridge tool (screenshot, `ui_*`, `control`) works with
//! no window. The automation crate stays free of egui; this app layer joins the two.

use effectcraft_automation::bridge::reply_result;
use effectcraft_automation::{ControlTransport, Error, Result};
use effectcraft_ui_egui::offscreen::{OffscreenOptions, OffscreenUi};
use serde_json::Value;

pub struct Offscreen(OffscreenUi);

impl Offscreen {
    pub fn start(
        opts: OffscreenOptions,
        make_session: impl FnOnce() -> std::result::Result<effectcraft_engine::Session, String> + Send + 'static,
    ) -> Result<Self> {
        OffscreenUi::start(opts, make_session).map(Offscreen).map_err(Error::Other)
    }

    pub fn adapter(&self) -> &str {
        self.0.adapter()
    }
}

impl ControlTransport for Offscreen {
    fn call(&mut self, method: &str, params: Value) -> Result<Value> {
        reply_result(self.0.call(method, params).map_err(Error::Other)?)
    }

    fn mode(&self) -> &'static str {
        "headless-ui"
    }
}
