//! `mcp --ui`: the MCP server with the full app offscreen. Screenshots and the `ui_*` tools work
//! with no window. Skips when there's no GPU adapter (CI without a GPU).
#![cfg(feature = "ui")]

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};

use effectcraft_automation::{base64, png_size};
use serde_json::{Value, json};

struct Server {
    child: Child,
    output: BufReader<ChildStdout>,
    next_id: u64,
}

impl Server {
    /// Start `mcp --ui …` and initialize it; `None` (skip) when no GPU adapter is usable.
    fn start(args: &[&str]) -> Option<(Self, Value)> {
        Self::start_in(args, None)
    }

    /// [`Server::start`] with `EFFECTCRAFT_CONFIG_DIR` set to `config`.
    fn start_in(args: &[&str], config: Option<&Path>) -> Option<(Self, Value)> {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_effectcraft-cli"));
        if let Some(dir) = config {
            cmd.env("EFFECTCRAFT_CONFIG_DIR", dir);
        }
        let mut child = cmd.args(["mcp", "--ui"]).args(args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
        let output = BufReader::new(child.stdout.take().unwrap());
        let mut s = Server { child, output, next_id: 1 };
        match s.try_rpc("initialize", json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "t", "version": "0"}})) {
            Some(r) => Some((s, r)),
            None => {
                let mut err = String::new();
                let _ = s.child.stderr.take().unwrap().read_to_string(&mut err);
                assert!(err.contains("no usable GPU adapter"), "the offscreen UI failed to start: {err}");
                eprintln!("no GPU adapter: skipping ({})", err.trim());
                None
            }
        }
    }

    fn try_rpc(&mut self, method: &str, params: Value) -> Option<Value> {
        let id = self.next_id;
        self.next_id += 1;
        let stdin = self.child.stdin.as_mut().unwrap();
        writeln!(stdin, "{}", json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})).ok()?;
        let mut line = String::new();
        if self.output.read_line(&mut line).unwrap() == 0 {
            return None;
        }
        let reply: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(reply["id"], id);
        assert!(reply.get("error").is_none(), "{method}: {reply}");
        Some(reply["result"].clone())
    }

    fn rpc(&mut self, method: &str, params: Value) -> Value {
        self.try_rpc(method, params).unwrap_or_else(|| panic!("{method}: the server exited"))
    }

    /// A tool call's content; panics if it failed.
    fn call(&mut self, name: &str, arguments: Value) -> Vec<Value> {
        let r = self.rpc("tools/call", json!({"name": name, "arguments": arguments}));
        assert_eq!(r["isError"], false, "{name}: {r}");
        r["content"].as_array().unwrap().clone()
    }

    fn json(&mut self, name: &str, arguments: Value) -> Value {
        let c = self.call(name, arguments);
        serde_json::from_str(c[0]["text"].as_str().unwrap()).unwrap()
    }

    /// Close stdin and wait for the server to exit.
    fn finish(mut self) -> std::process::ExitStatus {
        drop(self.child.stdin.take());
        self.child.wait().unwrap()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        drop(self.child.stdin.take());
        let _ = self.child.wait();
    }
}

fn element<'a>(elements: &'a Value, id: &str) -> Option<&'a Value> {
    elements.as_array()?.iter().find(|e| e["id"] == id)
}

#[test]
fn ui_tools_work_offscreen() {
    let Some((mut s, init)) = Server::start(&["--demo", "--ui-scale", "1"]) else { return };
    assert!(init["serverInfo"]["title"].as_str().unwrap().contains("headless-ui"), "{init}");
    let tools = s.rpc("tools/list", json!({}));
    for want in ["screenshot", "ui_elements", "ui_click", "ui_drag", "ui_key", "ui_type", "ui_set", "control"] {
        assert!(tools["tools"].as_array().unwrap().iter().any(|t| t["name"] == want), "{want} listed");
    }

    // The demo project is open in the app, through the same control channel.
    assert!(s.json("get_project", json!({}))["items"].as_array().is_some_and(|i| !i.is_empty()));

    // The UI is laid out: the Timeline panel and the tools are registered elements.
    let panels = s.json("ui_elements", json!({"prefix": "panel."}));
    let timeline = element(&panels, "panel.Timeline").unwrap_or_else(|| panic!("panel.Timeline in {panels}")).clone();
    let rect = timeline["rect"].as_array().unwrap().iter().map(|v| v.as_f64().unwrap()).collect::<Vec<_>>();
    assert!(rect[2] > 100.0 && rect[3] > 50.0, "Timeline rect {rect:?}");

    // A real click on a toolbar button switches the tool.
    let before = s.json("ui_inspect", json!({}))["ui"]["tool"].clone();
    let tools = s.json("ui_elements", json!({"prefix": "tools."}));
    let target = ["tools.Hand", "tools.Zoom", "tools.Rotation"]
        .into_iter()
        .find(|id| element(&tools, id).is_some())
        .unwrap_or_else(|| panic!("no Hand/Zoom/Rotation tool in {tools}"));
    s.json("ui_click", json!({"id": target}));
    let after = s.json("ui_inspect", json!({}))["ui"]["tool"].clone();
    assert_ne!(before, after, "clicking {target} changed the tool");
    assert_eq!(after.as_str().map(|t| target.ends_with(t)), Some(true), "{target} → {after}");

    // Keyboard input: V goes back to the Selection tool.
    s.json("ui_key", json!({"key": "V"}));
    assert_eq!(s.json("ui_inspect", json!({}))["ui"]["tool"], before);

    // A screenshot of one panel, as a PNG with the panel's shape (scale 1: points = pixels).
    let c = s.call("screenshot", json!({"panel": "Timeline"}));
    assert_eq!(c[0]["type"], "image");
    let png = base64::decode(c[0]["data"].as_str().unwrap()).unwrap();
    let (w, h) = png_size(&png).expect("a PNG");
    assert!((w as f64 - rect[2]).abs() <= 2.0 && (h as f64 - rect[3]).abs() <= 2.0, "{w}x{h} vs {rect:?}");
    // The whole window, downscaled to max_side.
    let c = s.call("screenshot", json!({"max_side": 800}));
    let (w, h) = png_size(&base64::decode(c[0]["data"].as_str().unwrap()).unwrap()).unwrap();
    assert!(w == 800 && h < 800, "{w}x{h}");
}

#[test]
fn ui_options_are_checked() {
    let bin = env!("CARGO_BIN_EXE_effectcraft-cli");
    for args in [&["info", "--ui"][..], &["mcp", "--ui", "--bridge", "9877"], &["mcp", "--ui-size", "800x600"], &["mcp", "--ui", "--ui-scale", "9"]] {
        let out = Command::new(bin).args(args).stdin(Stdio::null()).output().unwrap();
        assert_eq!(out.status.code(), Some(2), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }
}

/// `mcp --ui --autosave`: an edit made through the offscreen app is checkpointed (in the MCP
/// auto-save folder, before the reply) like headless work, and EOF ends the session's manifest.
#[test]
fn ui_autosave_checkpoints_the_offscreen_app() {
    let dir = std::env::temp_dir().join(format!("ec-mcp-ui-autosave-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let Some((mut s, init)) = Server::start_in(&["--autosave", "--ui-scale", "1"], Some(&dir)) else { return };
    let status = &init["_meta"]["effectcraftAutoSave"];
    assert_eq!(status["enabled"], true, "{init}");
    let folder = PathBuf::from(status["folder"].as_str().unwrap());
    assert!(folder.starts_with(dir.join("EffectCraft Auto-Save").join("MCP")), "{folder:?}");
    assert_eq!(status["latestPath"], Value::Null);

    // An edit through the app's control channel is on disk when its reply arrives.
    let r = s.rpc("tools/call", json!({"name": "command_run", "arguments": {"id": "comp.new", "params": {"name": "Saved Comp"}}}));
    assert_eq!(r["isError"], false, "{r}");
    let saved = r["_meta"]["effectcraftAutoSave"]["latestPath"].as_str().unwrap_or_else(|| panic!("no checkpoint: {r}")).to_string();
    assert!(Path::new(&saved).starts_with(&folder) && Path::new(&saved).is_file(), "{saved}");
    assert!(std::fs::read_to_string(&saved).unwrap().contains("Saved Comp"));
    let manifest: Value = serde_json::from_str(&std::fs::read_to_string(folder.join("session.json")).unwrap()).unwrap();
    assert_eq!(manifest["autosave"], saved.as_str());
    assert_eq!((manifest["dirty"].as_bool(), manifest["ended"].as_bool()), (Some(true), Some(false)), "{manifest}");
    // A query reports it too and doesn't write another version.
    assert_eq!(s.json("doc_inspect", json!({}))["project"]["autosave"]["latestPath"], saved.as_str());

    assert!(s.finish().success());
    let manifest: Value = serde_json::from_str(&std::fs::read_to_string(folder.join("session.json")).unwrap()).unwrap();
    assert_eq!(manifest["ended"], true, "{manifest}");
    assert_eq!(std::fs::read_dir(&folder).unwrap().count(), 2, "one checkpoint and the manifest");
    let _ = std::fs::remove_dir_all(&dir);
}
