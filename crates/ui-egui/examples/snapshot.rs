//! Headless UI snapshot: runs the real [`EffectcraftApp`](effectcraft_ui_egui::EffectcraftApp)
//! without a window (the [`offscreen`](effectcraft_ui_egui::offscreen) app that `effectcraft-cli
//! mcp --ui` serves) and writes PNGs. Works regardless of window focus, Spaces or display sleep, so
//! agents can look at the UI at any time.
//!
//! ```text
//! cargo run -p effectcraft-ui-egui --example snapshot -- [--out ui.png] [--size 1680x1020]
//!     [--scale 2] [--settle 1.5] [--demo | --empty] [--script steps.jsonl | --step '<json>']...
//! ```
//!
//! Each script step is a control-channel request (`{"method":"engine.execute","params":{...}}`,
//! see `docs/control-protocol.md`), run in order with the app settling in between. Extra methods:
//! `{"method":"snap","params":{"path":"x.png"}}` writes an intermediate snapshot;
//! `{"method":"pointer","params":{"x":237,"y":12,"click":true}}` moves the pointer to a window
//! point and clicks (`"down":true` presses and holds the button, later moves drag, `"up":true`
//! releases: snap mid-drag). Replies are printed to stdout as JSON lines.

use std::time::Duration;

use effectcraft_ui_egui::offscreen::{OffscreenOptions, OffscreenUi};
use serde_json::{Value, json};

struct Args {
    out: String,
    opts: OffscreenOptions,
    settle: f64,
    demo: bool,
    steps: Vec<Value>,
}

const USAGE: &str = "usage: snapshot [--out ui.png] [--size WxH] [--scale 2] [--settle secs] [--demo | --empty] [--script f.jsonl | --step json]...";

fn parse_args() -> Result<Args, String> {
    let mut a = Args { out: "ui.png".into(), opts: OffscreenOptions::default(), settle: 1.5, demo: true, steps: Vec::new() };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let mut val = || it.next().ok_or_else(|| format!("{arg} needs a value"));
        match arg.as_str() {
            "--out" => a.out = val()?,
            "--size" => {
                let v = val()?;
                let (w, h) = v.split_once('x').ok_or("--size WxH")?;
                a.opts.size = [w.parse().map_err(|_| "bad width")?, h.parse().map_err(|_| "bad height")?];
            }
            "--scale" => a.opts.pixels_per_point = val()?.parse().map_err(|_| "bad --scale")?,
            "--settle" => a.settle = val()?.parse().map_err(|_| "bad --settle")?,
            "--demo" => a.demo = true,
            "--empty" => a.demo = false,
            "--step" => a.steps.push(serde_json::from_str(&val()?).map_err(|e| format!("--step: {e}"))?),
            "--script" => {
                let path = val()?;
                let text = std::fs::read_to_string(&path).map_err(|e| format!("{path}: {e}"))?;
                for line in text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with("//")) {
                    a.steps.push(serde_json::from_str(line).map_err(|e| format!("{path}: {e}: {line}"))?);
                }
            }
            "-h" | "--help" => return Err(USAGE.into()),
            _ => return Err(format!("unknown argument {arg}\n{USAGE}")),
        }
    }
    if !(a.settle.is_finite() && (0.0..=600.0).contains(&a.settle)) {
        return Err("--settle: expected 0 to 600 seconds".into());
    }
    Ok(a)
}

/// Let background work (frame renders landing in the viewer) finish: the offscreen UI runs frames
/// on its own whenever the app asks for a repaint.
fn settle(secs: f64) {
    std::thread::sleep(Duration::from_secs_f64(secs));
}

/// `ui.screenshot` to `path`, printed as `{"snapshot", "width", "height"}`.
fn snap(ui: &OffscreenUi, path: &str) -> Result<(), String> {
    let r = ui.call("ui.screenshot", json!({"path": path}))?;
    if r["ok"] != json!(true) {
        return Err(format!("{path}: {}", r["error"].as_str().unwrap_or("screenshot failed")));
    }
    println!("{}", json!({"snapshot": path, "width": r["result"]["width"], "height": r["result"]["height"]}));
    Ok(())
}

/// The control request for a step: `pointer` is `ui.click` at its point, or `ui.move` (with
/// `down` / `up`).
fn request(method: &str, params: Value) -> (&str, Value) {
    if method != "pointer" {
        return (method, params);
    }
    let mut p = json!({"x": params["x"].as_f64().unwrap_or(0.0), "y": params["y"].as_f64().unwrap_or(0.0)});
    if params["click"].as_bool() == Some(true) {
        return ("ui.click", p);
    }
    for key in ["down", "up"] {
        if params[key].as_bool() == Some(true) {
            p[key] = json!(true);
        }
    }
    ("ui.move", p)
}

fn run(args: Args) -> Result<(), String> {
    let demo = args.demo;
    let ui = OffscreenUi::start(args.opts, move || {
        let mut session = effectcraft_host::session();
        if demo {
            session.execute_checked("file.openDemoProject", json!({})).map_err(|e| e.to_string())?;
        }
        Ok(session)
    })?;
    ui.wait_started()?;
    settle(args.settle);
    for step in &args.steps {
        let method = step["method"].as_str().unwrap_or_default();
        let params = step.get("params").cloned().unwrap_or(json!({}));
        if method == "snap" {
            if let Err(e) = snap(&ui, params["path"].as_str().unwrap_or("snap.png")) {
                eprintln!("{e}");
            }
            continue;
        }
        // The reply comes once the app answered and the UI settled.
        let (control, params) = request(method, params);
        let reply = ui.call(control, params).unwrap_or_else(|e| json!({"ok": false, "error": e}));
        println!("{}", json!({"method": method, "reply": reply}));
        settle(0.3);
    }
    settle(args.settle.min(1.0));
    snap(&ui, &args.out)
}

fn main() {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
    if let Err(e) = run(args) {
        eprintln!("{e}");
        std::process::exit(1);
    }
}
