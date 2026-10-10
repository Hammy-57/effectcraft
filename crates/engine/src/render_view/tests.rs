use serde_json::{Value, json};

use crate::Session;

/// A 320×180 comp ("Main", 4 s at 30 fps) with a 100×100 red solid at the centre (#2) under a
/// 40×40 green solid at (40, 40) (#1).
fn session() -> Session {
    let mut s = Session::default();
    s.execute("comp.new", json!({"name": "Main", "width": 320, "height": 180, "duration": 4, "frameRate": 30})).unwrap();
    s.execute("layer.newSolid", json!({"color": "#ff0000", "width": 100, "height": 100, "name": "Red"})).unwrap();
    let g = s.execute("layer.newSolid", json!({"color": "#00ff00", "width": 40, "height": 40, "name": "Green"})).unwrap()["layer"].clone();
    s.execute("prop.set", json!({"layer": g, "path": "transform/position", "value": [40, 40]})).unwrap();
    s
}

fn decode(v: &Value) -> image::RgbaImage {
    let b = effectcraft_track::roto::rle::base64_decode(v["png"].as_str().unwrap()).unwrap();
    image::load_from_memory(&b).unwrap().to_rgba8()
}

fn frame(s: &mut Session, p: Value) -> (Value, image::RgbaImage) {
    let mut p = p;
    p["base64"] = json!(true);
    let v = s.execute("render.frame", p).unwrap();
    let img = decode(&v);
    (v, img)
}

fn err(s: &mut Session, id: &str, p: Value) -> String {
    s.execute_checked(id, p).unwrap_err().to_string()
}

#[test]
fn render_frame_matches_the_comp() {
    let mut s = session();
    let (v, img) = frame(&mut s, json!({"time": 0}));
    assert_eq!((v["width"].as_u64(), v["height"].as_u64()), (Some(320), Some(180)));
    assert_eq!(img.dimensions(), (320, 180));
    assert_eq!(img.get_pixel(160, 90).0, [255, 0, 0, 255]);
    assert_eq!(img.get_pixel(40, 40).0, [0, 255, 0, 255]);
    assert_eq!(v["frame"], 0);
    let (_, img) = frame(&mut s, json!({"max_side": 160}));
    assert_eq!(img.dimensions(), (160, 90));
}

#[test]
fn region_renders_a_rectangle_at_full_resolution() {
    let mut s = session();
    let (v, img) = frame(&mut s, json!({"region": [100, 60, 80, 40]}));
    assert_eq!(img.dimensions(), (80, 40));
    assert_eq!(v["region"], json!([100.0, 60.0, 80.0, 40.0]));
    // (100, 60) is inside the red solid (110..210, 40..140) only from x = 110.
    assert_eq!(img.get_pixel(20, 10).0, [255, 0, 0, 255]);
    assert_ne!(img.get_pixel(2, 10).0, [255, 0, 0, 255]);
    // `max_side` applies to the region.
    let (_, img) = frame(&mut s, json!({"region": [100, 60, 80, 40], "max_side": 40}));
    assert_eq!(img.dimensions(), (40, 20));
    // Clipped to the frame; fractional edges round outward.
    let (v, img) = frame(&mut s, json!({"region": [300.5, -20, 100, 30]}));
    assert_eq!(img.dimensions(), (20, 10));
    assert_eq!(v["region"], json!([300.0, 0.0, 20.0, 10.0]));
}

#[test]
fn bad_regions_are_errors() {
    let mut s = session();
    for (r, want) in [
        (json!([400, 0, 10, 10]), "outside"),
        (json!([-50, -50, 10, 10]), "outside"),
        (json!([0, 0, 0, 10]), "positive"),
        (json!([0, 0, 10, -1]), "positive"),
        (json!([0, 0, 10]), "[x, y, width, height]"),
        (json!("0,0,10,10"), "[x, y, width, height]"),
        (json!([0, 0, "a", 10]), "[x, y, width, height]"),
        (json!([1e308, 1e308, 1e308, 1e308]), "outside"),
    ] {
        let e = err(&mut s, "render.frame", json!({"region": r, "base64": true}));
        assert!(e.contains(want), "{r}: {e}");
    }
    // Hostile but valid: a huge region is clipped to the frame.
    let (_, img) = frame(&mut s, json!({"region": [-1e300, -1e300, 1e308, 1e308], "max_side": 32}));
    assert_eq!(img.dimensions(), (32, 18));
}

#[test]
fn solo_layer_and_items() {
    let mut s = session();
    let (v, img) = frame(&mut s, json!({"layer": "Red"}));
    assert_eq!(img.get_pixel(160, 90).0, [255, 0, 0, 255]);
    assert_ne!(img.get_pixel(40, 40).0, [0, 255, 0, 255], "the green solid is not soloed");
    assert!(v["layer"].is_u64());
    // A hidden layer still renders when asked for by name.
    let cid = s.active_comp_id().unwrap();
    for l in std::sync::Arc::make_mut(&mut s.project).comp_mut(cid).unwrap().layers.iter_mut() {
        l.switches.video = l.name != "Green";
    }
    let (_, img) = frame(&mut s, json!({"layer": "#1", "transparent": true}));
    assert_eq!(img.get_pixel(40, 40).0, [0, 255, 0, 255]);
    assert_eq!(img.get_pixel(160, 90).0[3], 0);
    assert!(err(&mut s, "render.frame", json!({"layer": "Nope"})).contains("no layer"));
    assert!(err(&mut s, "render.frame", json!({"layer": 99})).contains("no layer"));

    // A solid item on its own, at its own size.
    let item = s.project.items.values().find(|i| matches!(i.kind, effectcraft_project::ItemKind::Solid(_)) && i.name.contains("Red")).map(|i| i.id.0);
    let (v, img) = frame(&mut s, json!({"item": item}));
    assert_eq!(img.dimensions(), (100, 100));
    assert_eq!(img.get_pixel(50, 50).0, [255, 0, 0, 255]);
    assert_eq!(v["item"].as_u64(), item);
    // A comp item is the comp.
    let (v, _) = frame(&mut s, json!({"item": "Main", "max_side": 16}));
    assert!(v["comp"].is_u64());
    assert!(err(&mut s, "render.frame", json!({"item": "Nope"})).contains("no project item"));
    assert!(err(&mut s, "render.frame", json!({"item": item, "layer": 1})).contains("needs a composition"));
    assert!(err(&mut s, "render.frame", json!({"item": item, "comp": "Main"})).contains("not both"));
    let folder = s.project.items.values().find(|i| matches!(i.kind, effectcraft_project::ItemKind::Folder)).map(|i| i.id.0);
    assert!(err(&mut s, "render.frame", json!({"item": folder})).contains("no pixels"));
}

#[test]
fn overlays_draw_and_report_layer_bounds() {
    let mut s = session();
    let (plain, a) = frame(&mut s, json!({"time": 0}));
    assert!(plain.get("layers").is_none());
    let (v, b) = frame(&mut s, json!({"time": 0, "overlays": true}));
    assert_eq!(v["overlays"], json!(["bounds", "anchor"]));
    let layers = v["layers"].as_array().unwrap();
    let red = layers.iter().find(|l| l["name"] == "Red").unwrap();
    assert_eq!(red["bounds"], json!([110.0, 40.0, 100.0, 100.0]));
    assert_eq!(red["anchor"], json!([160.0, 90.0]));
    assert_eq!(red["index"], 2);
    // Bounds draw on the box edge; the inside of the box is untouched.
    assert_ne!(a.get_pixel(110, 90), b.get_pixel(110, 90));
    assert_eq!(a.get_pixel(130, 120), b.get_pixel(130, 120));
    // A solo layer shows only its own overlay.
    let (v, _) = frame(&mut s, json!({"layer": "Green", "overlays": ["bounds"]}));
    assert_eq!(v["layers"].as_array().unwrap().len(), 1);
    // Every overlay at once, on a region, with a moving layer.
    s.execute("prop.addKey", json!({"layer": "Red", "path": "transform/position", "time": 0, "value": [60, 90]})).unwrap();
    s.execute("prop.addKey", json!({"layer": "Red", "path": "transform/position", "time": 3, "value": [260, 90]})).unwrap();
    let (v, img) = frame(&mut s, json!({"time": 1, "overlays": "all", "region": [0, 0, 160, 90]}));
    assert_eq!(img.dimensions(), (160, 90));
    let red = v["layers"].as_array().unwrap().iter().find(|l| l["name"] == "Red").unwrap().clone();
    assert_eq!(red["keyPositions"].as_array().unwrap().len(), 2);
    // Overlay names are checked.
    assert!(err(&mut s, "render.frame", json!({"overlays": ["bounds", "sparkles"]})).contains("unknown overlay"));
    assert!(err(&mut s, "render.frame", json!({"overlays": 3})).contains("overlays"));
    let o = super::Overlays::parse(Some(&json!(["Motion Path", "safe_margins", "GRID"]))).unwrap();
    assert!(o.motion_path && o.safe_margins && o.grid && !o.bounds);
}

#[test]
fn render_frame_rejects_bad_numbers_and_writes_files() {
    let mut s = session();
    assert!(err(&mut s, "render.frame", json!({"max_side": -1})).contains("max_side"));
    assert!(err(&mut s, "render.frame", json!({"time": "soon"})).contains("time"));
    assert!(err(&mut s, "render.frame", json!({"nope": 1})).contains("unknown parameter"));
    // Huge sizes are clamped, not trusted: 16384 on a 320-wide comp is still the comp size.
    let (_, img) = frame(&mut s, json!({"max_side": u64::MAX}));
    assert_eq!(img.dimensions(), (320, 180));
    let dir = std::env::temp_dir().join(format!("ec-render-view-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("f.png");
    let v = s.execute("render.frame", json!({"path": path.to_string_lossy(), "frame": 15})).unwrap();
    assert_eq!(v["time"], 0.5);
    assert!(v.get("png").is_none());
    assert_eq!(image::open(&path).unwrap().width(), 320);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn contact_sheet_spreads_frames_and_labels_them() {
    let mut s = session();
    s.execute("prop.addKey", json!({"layer": "Red", "path": "transform/position", "time": 0, "value": [60, 90]})).unwrap();
    s.execute("prop.addKey", json!({"layer": "Red", "path": "transform/position", "time": 3, "value": [260, 90]})).unwrap();
    let v = s.execute("render.contactSheet", json!({"count": 4, "base64": true, "max_side": 800})).unwrap();
    assert_eq!(v["count"], 4);
    assert_eq!(v["frames"], json!([0, 40, 79, 119]));
    let img = decode(&v);
    assert_eq!((img.width() as u64, img.height() as u64), (v["width"].as_u64().unwrap(), v["height"].as_u64().unwrap()));
    assert!(img.width() <= 800 && img.height() <= 800);
    let cell = v["cell"].as_array().unwrap();
    assert!(cell[0].as_u64().unwrap() <= 320 && cell[1].as_u64().unwrap() <= 180);
    // Labels draw light text on the dark strip below each cell.
    let (cw, ch) = (cell[0].as_u64().unwrap() as u32, cell[1].as_u64().unwrap() as u32);
    let strip = (4..4 + cw).flat_map(|x| (4 + ch..4 + ch + 18).map(move |y| (x, y)));
    assert!(strip.into_iter().any(|(x, y)| img.get_pixel(x, y).0[0] > 150), "label text in the strip");
    // Without labels: no strip.
    let v2 = s.execute("render.contactSheet", json!({"count": 4, "base64": true, "max_side": 800, "labels": false, "cols": 4})).unwrap();
    assert_eq!(v2["rows"], 1);
    assert_eq!(v2["cols"], 4);
    // Explicit times, a range, overlays and a solo layer.
    let v = s.execute("render.contactSheet", json!({"times": [0, 1.5, 3], "layer": "Red", "overlays": ["motionPath"], "base64": true})).unwrap();
    assert_eq!(v["times"], json!([0.0, 1.5, 3.0]));
    assert_eq!(v["overlays"], json!(["motionPath"]));
    let v = s.execute("render.contactSheet", json!({"start": 1, "end": 2, "count": 3, "base64": true})).unwrap();
    assert_eq!(v["frames"], json!([30, 45, 59]));
    // More frames than the range has: one per frame.
    let v = s.execute("render.contactSheet", json!({"start": 0, "end": 0.1, "count": 10, "base64": true})).unwrap();
    assert_eq!(v["frames"], json!([0, 1, 2]));
}

#[test]
fn contact_sheet_caps_and_rejects_hostile_input() {
    let mut s = session();
    let v = s.execute("render.contactSheet", json!({"count": u64::MAX, "max_side": 256, "base64": true})).unwrap();
    assert_eq!(v["count"], super::MAX_CELLS);
    assert!(v["note"].as_str().unwrap().contains("capped"));
    let many: Vec<f64> = (0..500).map(|i| i as f64 / 100.0).collect();
    let v = s.execute("render.contactSheet", json!({"times": many, "max_side": 256, "base64": true})).unwrap();
    assert_eq!(v["count"], super::MAX_CELLS);
    assert_eq!(v["requested"], 500);
    let v = s.execute("render.contactSheet", json!({"count": 2, "cols": 1000, "base64": true, "max_side": 1})).unwrap();
    assert_eq!(v["cols"], 2);
    for (p, want) in [
        (json!({"count": 0}), "at least 1"),
        (json!({"cols": 0}), "at least 1"),
        (json!({"times": []}), "empty"),
        (json!({"times": [0, "x"]}), "not a number"),
        (json!({"times": 3}), "list"),
        (json!({"start": 2, "end": 1}), "before"),
        (json!({"region": [0, 0, 0, 0]}), "positive"),
        (json!({"layer": "Nope"}), "no layer"),
        (json!({"time": 1}), "unknown parameter"),
    ] {
        let e = err(&mut s, "render.contactSheet", p.clone());
        assert!(e.contains(want), "{p}: {e}");
    }
}

#[test]
fn contact_sheet_writes_png_and_gif() {
    let mut s = session();
    let dir = std::env::temp_dir().join(format!("ec-contact-sheet-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (png, gif) = (dir.join("s.png"), dir.join("s.gif"));
    let v = s
        .execute("render.contactSheet", json!({"count": 3, "path": png.to_string_lossy(), "gif": gif.to_string_lossy(), "max_side": 400, "transparent": true}))
        .unwrap();
    assert_eq!(v["path"], json!(png.to_string_lossy()));
    assert!(v.get("png").is_none());
    assert_eq!(image::open(&png).unwrap().width() as u64, v["width"].as_u64().unwrap());
    let g = std::fs::read(&gif).unwrap();
    assert!(g.starts_with(b"GIF89a"));
    let _ = std::fs::remove_dir_all(dir);
}

/// Multi-agent fairness: `wait: false` renders as a background task on a copy of what the render
/// reads (the caller's comp and time included), so the session carries on meanwhile. The task's
/// result is the blocking command's, pixel for pixel, and it finishes without a toast.
#[test]
fn renders_run_as_quiet_background_tasks() {
    let mut s = session();
    s.execute("time.set", json!({"time": 1})).unwrap();
    for (cmd, p) in [("render.frame", json!({"base64": true, "max_side": 160})), ("render.contactSheet", json!({"count": 2, "base64": true, "max_side": 200}))] {
        let blocking = s.execute(cmd, p.clone()).unwrap();
        let mut bg = p.clone();
        bg["wait"] = json!(false);
        let started = s.execute(cmd, bg).unwrap();
        let job = started["job"].as_str().unwrap().to_string();
        assert!(s.jobs().iter().any(|j| j.id == job), "{cmd}: listed while it runs");
        s.events.clear();
        s.wait_jobs();
        let rec = s.job_log.iter().find(|r| r.id == job).unwrap();
        assert_eq!((rec.status.as_str(), rec.kind.as_str()), ("done", "render"));
        assert_eq!(rec.result, blocking, "{cmd}: the same result as the blocking command");
        assert!(rec.result.get("quiet").is_none());
        assert!(!s.events.iter().any(|e| matches!(e, crate::Event::Toast { .. })), "{cmd}: no toast for an agent's render");
    }
    // A bad request fails up front, not in the task.
    assert!(s.execute("render.frame", json!({"wait": false, "layer": "Nope"})).is_err());
}
