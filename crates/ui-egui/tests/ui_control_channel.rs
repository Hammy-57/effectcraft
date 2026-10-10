//! Control channel robustness: cancelled (timed-out) requests never run, `events.subscribe`
//! change events, and `ui.screenshot`'s `max_side`.

use std::sync::mpsc::TryRecvError;

use effectcraft_engine::Session;
use effectcraft_ui_egui::control::{self, ControlEvents};
use effectcraft_ui_egui::{ControlRequest, EffectcraftApp};
use egui_kittest::Harness;
use serde_json::{Value, json};

/// Regression: the server replied "timeout" but the UI thread still ran the request later, so a
/// client's retry created a duplicate.
#[test]
fn a_cancelled_request_is_skipped() {
    let (tx, rx) = std::sync::mpsc::channel();
    let mut h = Harness::builder().with_size(egui::vec2(1280.0, 800.0)).build_eframe(|_| EffectcraftApp::new(Session::default()).with_control(rx));
    h.run_steps(2);
    let before = h.state().session.revision;
    let (req, reply) = ControlRequest::new("engine.execute", json!({"command": "comp.new", "params": {"name": "Late"}}));
    assert!(req.state.cancel(), "not started yet");
    tx.send(req).unwrap();
    h.run_steps(3);
    assert_eq!(h.state().session.revision, before, "a cancelled request must not run");
    assert_eq!(reply.try_recv().unwrap_err(), TryRecvError::Disconnected);
    // A live one runs, and can't be cancelled once started.
    let (req, reply) = ControlRequest::new("engine.execute", json!({"command": "comp.new", "params": {"name": "Now"}}));
    let state = req.state.clone();
    tx.send(req).unwrap();
    h.run_steps(2);
    assert_eq!(reply.try_recv().unwrap()["ok"], true);
    assert!(!state.cancel());
    assert!(h.state().session.revision > before);
}

fn next_event(rx: &std::sync::mpsc::Receiver<Value>, h: &mut Harness<'_, EffectcraftApp>) -> Value {
    for _ in 0..40 {
        if let Ok(v) = rx.try_recv() {
            return v;
        }
        h.step();
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    panic!("no event");
}

#[test]
fn subscribers_hear_project_and_selection_changes() {
    let (tx, rx) = std::sync::mpsc::channel();
    let events = ControlEvents::default();
    let hub = events.clone();
    let mut h = Harness::builder()
        .with_size(egui::vec2(1280.0, 800.0))
        .build_eframe(move |_| EffectcraftApp::new(Session::default()).with_control(rx).with_control_events(hub));
    h.run_steps(2);
    let (sub, revision) = events.subscribe();
    assert_eq!(revision, h.state().session.revision);
    // Nothing changed: nothing is sent.
    h.run_steps(3);
    assert!(sub.try_recv().is_err());

    let (req, reply) = ControlRequest::new("engine.execute", json!({"command": "comp.new", "params": {"name": "E"}}));
    tx.send(req).unwrap();
    h.run_steps(2);
    assert_eq!(reply.try_recv().unwrap()["ok"], true);
    let ev = next_event(&sub, &mut h);
    assert_eq!(ev["event"], "project.changed", "{ev}");
    assert_eq!(ev["revision"], h.state().session.revision);

    // An edit from the app itself (not the control channel) is published too.
    let layer = h.state_mut().session.execute("layer.newSolid", json!({"color": "#336699"})).unwrap();
    let mut seen = vec![];
    while seen.len() < 4 {
        let ev = next_event(&sub, &mut h);
        let done = ev["event"] == "selection.changed";
        seen.push(ev);
        if done {
            break;
        }
    }
    assert!(seen.iter().any(|e| e["event"] == "project.changed"), "{seen:?}");
    let sel = seen.iter().find(|e| e["event"] == "selection.changed").expect("selection.changed");
    assert_eq!(sel["layers"], json!([layer["layer"].as_u64().or(layer.as_u64()).unwrap()]), "{sel} {layer}");

    // A dropped subscriber is forgotten.
    drop(sub);
    h.state_mut().session.execute("comp.new", json!({})).unwrap();
    h.run_steps(20);
    assert_eq!(events.subscribers(), 0);
}

/// A full 4K window was a 31 MB PNG: screenshots are downscaled to `max_side` (default 1600)
/// and compressed.
#[test]
fn screenshots_are_capped_and_compressed() {
    let (w, h) = (3840u32, 2036u32);
    let rgba: Vec<u8> = (0..w * h).flat_map(|i| [(i % 251) as u8, (i / w % 256) as u8, 40, 255]).collect();
    let (small, sw, sh) = control::shrink(rgba.clone(), w, h, control::SCREENSHOT_MAX_SIDE);
    assert_eq!((sw, sh), (1600, 848));
    assert_eq!(small.len(), (sw * sh * 4) as usize);
    let png = control::encode_png_fast(&small, sw, sh).unwrap();
    assert!(png.len() < 2 * 1024 * 1024, "{} bytes", png.len());
    assert_eq!(image::load_from_memory(&png).unwrap().width(), 1600);
    // 0 keeps the full size.
    let (_, fw, fh) = control::shrink(rgba, w, h, 0);
    assert_eq!((fw, fh), (w, h));
}

/// Multi-agent fairness: on desktop a `wait: true` job command no longer blocks the UI thread
/// (and with it every other client's requests). It starts the job, the reply waits for it, and
/// the reply is what the blocking command returns. `jobs.wait` waits the same way.
#[test]
fn waiting_job_commands_reply_when_the_job_ends_without_blocking() {
    let (tx, rx) = std::sync::mpsc::channel();
    let mut h = Harness::builder().with_size(egui::vec2(1280.0, 800.0)).build_eframe(|_| EffectcraftApp::new(Session::default()).with_control(rx));
    h.run_steps(2);
    let run = |h: &mut Harness<'_, EffectcraftApp>, command: &str, params: Value| {
        let (req, reply) = ControlRequest::new("engine.execute", json!({"command": command, "params": params}));
        tx.send(req).unwrap();
        h.run_steps(1);
        reply
    };
    run(&mut h, "comp.new", json!({"name": "Main", "width": 64, "height": 36})).recv().unwrap();
    run(&mut h, "layer.newSolid", json!({"name": "S", "width": 8, "height": 8})).recv().unwrap();
    let mut blocking = Session::default();
    blocking.execute_checked("comp.new", json!({"name": "Main", "width": 64, "height": 36})).unwrap();
    blocking.execute_checked("layer.newSolid", json!({"name": "S", "width": 8, "height": 8})).unwrap();
    let expected = blocking.execute_checked("footage.check", json!({"wait": true})).unwrap();

    let waiting = run(&mut h, "footage.check", json!({"wait": true}));
    // Another client's request is answered while the job runs (or right after it).
    let other = run(&mut h, "comp.new", json!({"name": "Meanwhile"}));
    let mut reply = None;
    for _ in 0..200 {
        if let Ok(v) = waiting.try_recv() {
            reply = Some(v);
            break;
        }
        h.run_steps(1);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let reply = reply.expect("the waiting command replied once its job ended");
    assert_eq!(reply, json!({"ok": true, "result": expected}));
    assert_eq!(other.recv().unwrap()["ok"], true);
    assert!(h.state().session.jobs().is_empty());

    // jobs.wait with a task running replies when it is done, with the finished jobs.
    run(&mut h, "footage.check", json!({})).recv().unwrap();
    let waited = run(&mut h, "jobs.wait", json!({}));
    let mut finished = None;
    for _ in 0..200 {
        if let Ok(v) = waited.try_recv() {
            finished = Some(v);
            break;
        }
        h.run_steps(1);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let finished = finished.expect("jobs.wait replied");
    assert_eq!(finished["ok"], true, "{finished}");
    assert!(finished["result"]["finished"].as_array().is_some_and(|a| a.iter().any(|j| j["kind"] == "footageCheck")), "{finished}");
}
