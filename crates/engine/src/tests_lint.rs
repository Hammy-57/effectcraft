//! `render.lint`: each design rule on a tiny synthetic comp, a clean comp, merging occurrences
//! over a range, sampling, and the beat matching behind `off-beat`.

use effectcraft_project::LayerId;
use serde_json::{Value, json};

use crate::Session;
use crate::lint::{self, KeyMoment, Severity};

/// A 320×180 comp, black background, `dur` seconds at 25 fps.
fn comp(dur: f64) -> Session {
    let mut s = Session::default();
    s.execute("comp.new", json!({"name": "Lint", "width": 320, "height": 180, "frameRate": 25, "duration": dur})).unwrap();
    s
}

fn solid(s: &mut Session, name: &str, w: u32, h: u32, color: &str) -> u64 {
    s.execute("layer.newSolid", json!({"name": name, "width": w, "height": h, "color": color})).unwrap()["layer"].as_u64().unwrap()
}

fn text(s: &mut Session, name: &str, t: &str, size: f64, fill: &str, pos: [f64; 2]) -> u64 {
    s.execute("layer.newText", json!({"name": name, "text": t, "size": size, "fill": fill, "position": pos})).unwrap()["layer"].as_u64().unwrap()
}

fn set(s: &mut Session, layer: u64, path: &str, value: Value) {
    s.execute("prop.set", json!({"layer": layer, "path": path, "value": value})).unwrap();
}

fn lint(s: &mut Session, p: Value) -> Value {
    s.execute("render.lint", p).unwrap()
}

/// The problems of `rule`.
fn of(r: &Value, rule: &str) -> Vec<Value> {
    r["problems"].as_array().unwrap().iter().filter(|p| p["rule"] == rule).cloned().collect()
}

/// The one problem of `rule` (fails when there are none or several).
fn one(r: &Value, rule: &str) -> Value {
    let v = of(r, rule);
    assert_eq!(v.len(), 1, "{rule}: {r:#}");
    v[0].clone()
}

#[test]
fn clean_comp_has_no_problems() {
    let mut s = comp(2.0);
    solid(&mut s, "BG", 320, 180, "#102040");
    text(&mut s, "Title", "Hello", 24.0, "#ffffff", [160.0, 90.0]);
    let r = lint(&mut s, json!({"time": 0.5}));
    assert_eq!(r["problems"], json!([]), "{r:#}");
    assert_eq!(r["clean"], true);
    assert_eq!(r["counts"], json!({"error": 0, "warning": 0, "info": 0}));
    assert_eq!(r["compName"], "Lint");
    assert_eq!(r["times"], json!([0.5]));
    assert_eq!(r["truncated"], false);
    assert!(r["notes"][0].as_str().unwrap().contains("no audible layers"), "{r}");
}

#[test]
fn text_too_small_warns_then_errors() {
    let mut s = comp(2.0);
    // 180 px tall: the minimum is 6 px.
    let t = text(&mut s, "Small", "Hello", 5.0, "#ffffff", [160.0, 90.0]);
    let r = lint(&mut s, json!({"time": 0, "rules": ["text-too-small"]}));
    let p = one(&r, "text-too-small");
    assert_eq!((p["severity"].as_str(), p["layer"].as_u64(), p["time"].as_f64()), (Some("warning"), Some(t), Some(0.0)), "{p}");
    assert_eq!(p["layerName"], "Small");
    assert!(p["message"].as_str().unwrap().contains("5.0 px"), "{p}");
    assert_eq!(r["clean"], false);
    // Under half the minimum: an error. The layer's scale counts (font size 20 at 10 % = 2 px).
    s.execute("layer.setText", json!({"layer": t, "size": 20})).unwrap();
    set(&mut s, t, "transform/scale", json!([10.0, 10.0, 100.0]));
    let p = one(&lint(&mut s, json!({"time": 0, "rules": ["text-too-small"]})), "text-too-small");
    assert_eq!(p["severity"], "error", "{p}");
    assert!(p["message"].as_str().unwrap().contains("font size 20"), "{p}");
    // minTextPx overrides the threshold.
    set(&mut s, t, "transform/scale", json!([100.0, 100.0, 100.0]));
    assert!(of(&lint(&mut s, json!({"time": 0, "rules": ["text-too-small"]})), "text-too-small").is_empty());
    let p = one(&lint(&mut s, json!({"time": 0, "rules": ["text-too-small"], "minTextPx": 30})), "text-too-small");
    assert_eq!(p["severity"], "warning", "{p}");
}

#[test]
fn low_contrast_measures_what_is_under_the_text() {
    let mut s = comp(2.0);
    solid(&mut s, "Grey", 320, 180, "#808080");
    let t = text(&mut s, "Faint", "Hello", 30.0, "#8a8a8a", [160.0, 90.0]);
    let r = lint(&mut s, json!({"time": 0, "rules": ["low-contrast"]}));
    let p = one(&r, "low-contrast");
    assert_eq!((p["severity"].as_str(), p["layer"].as_u64()), (Some("error"), Some(t)), "{p}");
    let m = p["message"].as_str().unwrap();
    assert!(m.contains("#8a8a8a") && m.contains("#808080") && m.contains(":1"), "{m}");
    // #c0c0c0 on #808080 is about 2.2:1: a warning.
    s.execute("layer.setText", json!({"layer": t, "fill": "#c0c0c0"})).unwrap();
    assert_eq!(one(&lint(&mut s, json!({"time": 0, "rules": ["low-contrast"]})), "low-contrast")["severity"], "warning");
    // White on mid grey is about 4:1: fine.
    s.execute("layer.setText", json!({"layer": t, "fill": "#ffffff"})).unwrap();
    assert!(of(&lint(&mut s, json!({"time": 0, "rules": ["low-contrast"]})), "low-contrast").is_empty());
    // The layer's opacity blends the text into the background.
    set(&mut s, t, "transform/opacity", json!(20.0));
    assert_eq!(one(&lint(&mut s, json!({"time": 0, "rules": ["low-contrast"]})), "low-contrast")["severity"], "error");
}

#[test]
fn safe_areas_and_cut_off() {
    let mut s = comp(2.0);
    // A title 20 px from the left edge: on screen but outside title safe (32 px per side).
    let t = text(&mut s, "Edge", "Hi", 24.0, "#ffffff", [20.0, 90.0]);
    let r = lint(&mut s, json!({"time": 0}));
    let p = one(&r, "outside-title-safe");
    assert_eq!((p["severity"].as_str(), p["layer"].as_u64()), (Some("warning"), Some(t)), "{r:#}");
    assert!(p["message"].as_str().unwrap().contains("on the left"), "{p}");
    assert!(of(&r, "cut-off").is_empty() && of(&r, "outside-action-safe").is_empty(), "{r:#}");
    s.execute("edit.clear", json!({"layers": [t]})).unwrap();

    // A box touching the left edge: outside action safe (16 px), not cut off.
    let b = solid(&mut s, "Box", 40, 40, "#ff0000");
    set(&mut s, b, "transform/position", json!([20.0, 90.0, 0.0]));
    let r = lint(&mut s, json!({"time": 0}));
    let p = one(&r, "outside-action-safe");
    assert_eq!((p["severity"].as_str(), p["layer"].as_u64()), (Some("info"), Some(b)), "{r:#}");
    assert!(of(&r, "cut-off").is_empty(), "{r:#}");
    // Half off the left edge: cut off (info for a solid).
    set(&mut s, b, "transform/position", json!([0.0, 90.0, 0.0]));
    let p = one(&lint(&mut s, json!({"time": 0})), "cut-off");
    assert_eq!((p["severity"].as_str(), p["layer"].as_u64()), (Some("info"), Some(b)), "{p}");
    assert!(p["message"].as_str().unwrap().contains("50% of it is outside") && p["message"].as_str().unwrap().contains("left"), "{p}");
    // Cut-off text is a warning.
    let t = text(&mut s, "Wide", "Hello", 24.0, "#ffffff", [310.0, 90.0]);
    let r = lint(&mut s, json!({"time": 0, "rules": ["cut-off"]}));
    let p = of(&r, "cut-off").into_iter().find(|p| p["layer"] == t).unwrap();
    assert_eq!(p["severity"], "warning", "{p}");
    assert_eq!(r["rules"], json!(["cut-off"]));
    // A full-frame background is neither cut off nor outside the safe areas.
    let mut s = comp(2.0);
    let bg = solid(&mut s, "BG", 400, 300, "#204060");
    let r = lint(&mut s, json!({"time": 0}));
    assert!(r["problems"].as_array().unwrap().iter().all(|p| p["layer"] != bg), "{r:#}");
}

#[test]
fn text_overlap_names_both_layers() {
    let mut s = comp(2.0);
    let a = text(&mut s, "One", "Hello", 24.0, "#ffffff", [160.0, 90.0]);
    let b = text(&mut s, "Two", "World", 24.0, "#ffffff", [165.0, 95.0]);
    let r = lint(&mut s, json!({"time": 0}));
    let p = one(&r, "text-overlap");
    // The upper layer (created last) first, the other as `other`.
    assert_eq!((p["severity"].as_str(), p["layer"].as_u64(), p["other"].as_u64()), (Some("warning"), Some(b), Some(a)), "{p}");
    assert_eq!(p["otherName"], "One");
    set(&mut s, b, "transform/position", json!([160.0, 150.0, 0.0]));
    assert!(of(&lint(&mut s, json!({"time": 0})), "text-overlap").is_empty());
}

#[test]
fn hidden_visible_reasons() {
    let mut s = comp(2.0);
    let off = solid(&mut s, "Away", 40, 40, "#ff0000");
    set(&mut s, off, "transform/position", json!([1000.0, 90.0, 0.0]));
    let r = lint(&mut s, json!({"time": 0, "rules": ["hidden-visible"]}));
    let p = one(&r, "hidden-visible");
    assert_eq!((p["severity"].as_str(), p["layer"].as_u64()), (Some("info"), Some(off)), "{p}");
    assert!(p["message"].as_str().unwrap().contains("entirely outside"), "{p}");
    // Info doesn't make the comp unclean.
    assert_eq!(r["clean"], true);
    s.execute("edit.clear", json!({"layers": [off]})).unwrap();

    let faded = solid(&mut s, "Faded", 40, 40, "#ff0000");
    set(&mut s, faded, "transform/opacity", json!(0.0));
    let p = one(&lint(&mut s, json!({"time": 0, "rules": ["hidden-visible"]})), "hidden-visible");
    assert!(p["layer"] == faded && p["message"].as_str().unwrap().contains("opacity is 0"), "{p}");
    s.execute("edit.clear", json!({"layers": [faded]})).unwrap();

    let under = solid(&mut s, "Under", 40, 40, "#ff0000");
    let cover = solid(&mut s, "Cover", 320, 180, "#000000");
    let p = one(&lint(&mut s, json!({"time": 0, "rules": ["hidden-visible"]})), "hidden-visible");
    assert_eq!((p["layer"].as_u64(), p["other"].as_u64()), (Some(under), Some(cover)), "{p}");
    assert!(p["message"].as_str().unwrap().contains("covered by “Cover”"), "{p}");
    // A half-transparent cover hides nothing.
    set(&mut s, cover, "transform/opacity", json!(50.0));
    assert!(of(&lint(&mut s, json!({"time": 0, "rules": ["hidden-visible"]})), "hidden-visible").is_empty());
}

#[test]
fn range_merges_occurrences() {
    let mut s = comp(2.0);
    let b = solid(&mut s, "Mover", 40, 40, "#ff0000");
    s.execute("prop.addKey", json!({"layer": b, "path": "transform/position", "value": [160.0, 90.0, 0.0], "time": 0.0})).unwrap();
    s.execute("prop.addKey", json!({"layer": b, "path": "transform/position", "value": [320.0, 90.0, 0.0], "time": 1.0})).unwrap();
    let r = lint(&mut s, json!({"start": 0, "end": 2, "rules": ["cut-off"]}));
    // Every 0.5 s, the end clamped to the last frame (1.96 s), keyframes at 0 and 1 s.
    assert_eq!(r["times"], json!([0.0, 0.5, 1.0, 1.5, 1.96]), "{r}");
    let p = one(&r, "cut-off");
    assert_eq!((p["layer"].as_u64(), p["time"].as_f64(), p["until"].as_f64()), (Some(b), Some(1.0), Some(1.96)), "{p}");
    // Keyframes add sample times between the steps.
    s.execute("prop.addKey", json!({"layer": b, "path": "transform/opacity", "value": 50.0, "time": 0.2})).unwrap();
    let r = lint(&mut s, json!({"start": 0, "end": 1, "rules": ["cut-off"]}));
    assert_eq!(r["times"], json!([0.0, 0.2, 0.5, 1.0]), "{r}");
    // Long ranges are thinned to 120 samples.
    let mut s = comp(10.0);
    solid(&mut s, "Box", 40, 40, "#ff0000");
    let r = lint(&mut s, json!({"start": 0, "end": 10, "step": 0.04, "rules": ["cut-off"]}));
    assert_eq!((r["truncated"].as_bool(), r["times"].as_array().unwrap().len()), (Some(true), 120), "{r}");
    assert_eq!(r["times"][0], 0.0);
    assert_eq!(r["times"][119], 9.96);
}

#[test]
fn bad_params_are_errors() {
    let mut s = comp(2.0);
    for p in [
        json!({"rules": ["nope"]}),
        json!({"time": 1, "start": 0}),
        json!({"start": 1, "end": 0}),
        json!({"start": 0, "step": 0}),
        json!({"minTextPx": -1}),
        json!({"minContrast": 30}),
        json!({"colour": 1}),
    ] {
        assert!(s.execute_checked("render.lint", p.clone()).is_err(), "{p}");
    }
    assert!(s.execute_checked("render.lint", json!({"start": 0, "end": 1, "step": 0.5, "rules": ["cut-off"], "minTextPx": 9, "minContrast": 4.5})).is_ok());
    let e = s.execute("render.lint", json!({"rules": ["nope"]})).unwrap_err().to_string();
    assert!(e.contains("text-too-small"), "{e}");
}

#[test]
fn off_beat_matches_key_moments_to_beats() {
    let mut s = comp(2.0);
    let l = solid(&mut s, "Logo", 10, 10, "#ffffff");
    let comp = s.project.comp(s.active_comp_id().unwrap()).unwrap().clone();
    let beats = [0.0, 0.5, 1.0, 1.5, 2.0];
    let m = |t: f64| KeyMoment { layer: LayerId(l), time: t, what: "Position keyframe".into() };
    // On the beat, one frame off (allowed), 3 frames late, exactly between beats, 2.5 frames early.
    let found = lint::off_beat(&comp, &[m(1.0), m(1.04), m(1.12), m(1.25), m(0.9)], &beats, 25.0);
    let got: Vec<(f64, Severity, bool)> = found.iter().map(|(t, f)| (t.seconds(), f.severity, f.message.contains("after"))).collect();
    assert_eq!(got.len(), 2, "{found:#?}");
    assert!((got[0].0 - 1.12).abs() < 1e-6 && got[0].2, "{found:#?}");
    assert!((got[1].0 - 0.9).abs() < 1e-6 && !got[1].2, "{found:#?}");
    assert!(found[0].1.message.contains("3 frames after the beat at 1.000 s"), "{}", found[0].1.message);
    assert!(found[1].1.message.contains("2.5 frames before"), "{}", found[1].1.message);
    assert!(lint::off_beat(&comp, &[m(1.12)], &[1.0], 25.0).is_empty(), "one beat is no grid");
}

#[test]
fn beats_of_a_click_track() {
    // 0.02 s bursts every 0.5 s from 0.25 s, 6 s at 12 kHz.
    let rate = 12_000u32;
    let mut mono = vec![0.0f32; 6 * rate as usize];
    for k in 0..12 {
        let at = (0.25 + 0.5 * k as f64) * rate as f64;
        for i in 0..240 {
            if let Some(v) = mono.get_mut(at as usize + i) {
                *v = 0.5 * (i as f32 * 0.5).sin();
            }
        }
    }
    let beats = lint::detect_beats(&mono, rate);
    assert_eq!(beats.len(), 12, "{beats:?}");
    for (k, b) in beats.iter().enumerate() {
        assert!((b - (0.25 + 0.5 * k as f64)).abs() <= 0.006, "{beats:?}");
    }
    // Silence and a steady tone have no beat.
    assert!(lint::detect_beats(&vec![0.0; 12_000], rate).is_empty());
    let tone: Vec<f32> = (0..36_000).map(|i| 0.5 * (i as f32 * 0.5).sin()).collect();
    assert!(lint::detect_beats(&tone, rate).is_empty());
}

#[test]
fn off_beat_on_a_comp_with_clicks() {
    let mut s = comp(4.0);
    // Clicks: one-frame 1 kHz Tone solids every 12 frames (0.48 s, 125 BPM).
    for k in 0..8 {
        let c = solid(&mut s, &format!("Click {k}"), 10, 10, "#000000");
        s.execute("effect.apply", json!({"layers": [c], "effect": "Tone"})).unwrap();
        for (p, v) in [("frequency1", 1000.0), ("frequency2", 0.0), ("frequency3", 0.0), ("frequency4", 0.0), ("frequency5", 0.0), ("level", 50.0)] {
            set(&mut s, c, &format!("effects/#1/{p}"), json!(v));
        }
        s.execute("layer.timing", json!({"layers": [c], "in": 0.48 * k as f64, "out": 0.48 * k as f64 + 0.04})).unwrap();
    }
    // A logo keyed on the beat at 0.96 s and 3 frames after it.
    let logo = solid(&mut s, "Logo", 20, 20, "#ffffff");
    s.execute("prop.addKey", json!({"layer": logo, "path": "transform/position", "value": [100.0, 90.0, 0.0], "time": 0.96})).unwrap();
    s.execute("prop.addKey", json!({"layer": logo, "path": "transform/position", "value": [200.0, 90.0, 0.0], "time": 1.08})).unwrap();
    let r = lint(&mut s, json!({"start": 0, "end": 4, "rules": ["off-beat"]}));
    let p = one(&r, "off-beat");
    assert_eq!((p["severity"].as_str(), p["layer"].as_u64()), (Some("warning"), Some(logo)), "{r:#}");
    assert!((p["time"].as_f64().unwrap() - 1.08).abs() < 1e-6 && p["until"] == p["time"], "{p}");
    assert!(p["message"].as_str().unwrap().contains("3 frames after the beat at 0.960 s"), "{p}");
    assert!(r["notes"][0].as_str().unwrap().contains("125 BPM"), "{r}");
    // One time checks the key moments on that frame only.
    assert_eq!(of(&lint(&mut s, json!({"time": 1.08, "rules": ["off-beat"]})), "off-beat").len(), 1);
    assert!(of(&lint(&mut s, json!({"time": 2.0, "rules": ["off-beat"]})), "off-beat").is_empty());
}
