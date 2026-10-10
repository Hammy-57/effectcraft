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
    assert_eq!(red["bounds"], json!([110.0, 40.0, 210.0, 140.0]));
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
