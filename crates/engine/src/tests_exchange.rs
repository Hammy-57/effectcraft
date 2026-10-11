//! Library exchange: exported layers (with the parents and mattes they need) come back into
//! another comp at the playhead in one undo step; a comp exports with what it uses; animation
//! presets keep an effect's keyframes relative to the preset's first key.

use serde_json::json;

use crate::Session;

fn out_dir(name: &str) -> std::path::PathBuf {
    let d = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/test-out").join(name);
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn layer_id(v: serde_json::Value) -> u64 {
    v["layer"].as_u64().unwrap()
}

#[test]
fn exported_layers_import_into_another_comp_at_the_playhead() {
    let dir = out_dir("exchange-layers");
    let path = dir.join("card.ecproj").to_string_lossy().to_string();
    let mut s = Session::default();
    s.execute("comp.new", json!({"name": "Inner", "width": 64, "height": 64, "frameRate": 30, "duration": 4})).unwrap();
    let inner = s.active_comp_id().unwrap();
    s.execute("layer.newSolid", json!({"name": "Fill", "color": "#ff0000"})).unwrap();
    s.execute("comp.new", json!({"name": "Main", "width": 64, "height": 64, "frameRate": 30, "duration": 4})).unwrap();
    let main = s.active_comp_id().unwrap();
    let nest = layer_id(s.execute("layer.addItem", json!({"comp": main.0, "item": inner.0, "time": 1.0})).unwrap());
    let rig = layer_id(s.execute("layer.newNull", json!({"name": "Rig"})).unwrap());
    let other = layer_id(s.execute("layer.newSolid", json!({"name": "Unrelated", "color": "#00ff00"})).unwrap());
    s.execute("layer.setParent", json!({"layers": [nest], "parent": rig})).unwrap();
    let before = s.project.items.len();

    let r = s.execute("file.exportLayers", json!({"path": path, "layers": [nest]})).unwrap();
    assert_eq!(r["layers"].as_array().unwrap().len(), 1);
    assert_eq!(r["included"], 2, "the parent travels with the layer");
    assert_eq!(s.project.items.len(), before, "export leaves the open project alone");
    assert!(s.project.comp(main).unwrap().layer(effectcraft_project::LayerId(other)).is_some());

    s.execute("comp.new", json!({"name": "Target", "width": 64, "height": 64, "frameRate": 30, "duration": 4})).unwrap();
    let target = s.active_comp_id().unwrap();
    s.execute("time.set", json!({"time": 2.0})).unwrap();
    let r = s.execute("file.importLayers", json!({"path": path})).unwrap();
    let ids: Vec<u64> = r["layers"].as_array().unwrap().iter().map(|v| v.as_u64().unwrap()).collect();
    assert_eq!(ids.len(), 2);
    let c = s.project.comp(target).unwrap();
    assert_eq!(c.layers.len(), 2);
    let names: Vec<String> = c.layers.iter().map(|l| l.name.clone()).collect();
    assert!(names.iter().any(|n| n == "Rig"), "{names:?}");
    let nested = c.layers.iter().find(|l| l.source.item().is_some()).unwrap();
    let rig_new = c.layers.iter().find(|l| l.name == "Rig").unwrap();
    assert_eq!(nested.parent, Some(rig_new.id), "parenting survives");
    assert!(nested.in_point.seconds() >= 2.0 - 1e-6 && rig_new.in_point.seconds() >= 2.0 - 1e-6);
    let first = c.layers.iter().map(|l| l.in_point).min().unwrap();
    assert!((first.seconds() - 2.0).abs() < 1e-6, "the earliest layer starts at the playhead");
    let src = nested.source.item().unwrap();
    assert!(s.project.comp(src).is_some(), "the nested comp came along");
    assert_ne!(src, inner, "it is a fresh copy");

    s.execute("edit.undo", json!({})).unwrap();
    assert!(s.project.comp(target).unwrap().layers.is_empty());
    assert_eq!(s.project.items.len(), before + 1, "one undo removes the layers and their items");
}

#[test]
fn exported_comp_holds_what_it_uses() {
    let dir = out_dir("exchange-comp");
    let path = dir.join("comp.ecproj").to_string_lossy().to_string();
    let mut s = Session::default();
    s.execute("comp.new", json!({"name": "Inner", "width": 64, "height": 64, "frameRate": 30, "duration": 2})).unwrap();
    let inner = s.active_comp_id().unwrap();
    s.execute("comp.new", json!({"name": "Spare", "width": 64, "height": 64, "frameRate": 30, "duration": 2})).unwrap();
    s.execute("comp.new", json!({"name": "Main", "width": 64, "height": 64, "frameRate": 30, "duration": 2})).unwrap();
    let main = s.active_comp_id().unwrap();
    s.execute("layer.addItem", json!({"comp": main.0, "item": inner.0, "time": 0.0})).unwrap();
    let r = s.execute("file.exportComp", json!({"path": path})).unwrap();
    assert_eq!(r["comp"], "Main");
    let text = std::fs::read_to_string(&path).unwrap();
    let p = effectcraft_project::Project::from_json(&text).unwrap();
    let names: Vec<String> = p.items.values().map(|i| i.name.clone()).collect();
    assert!(names.contains(&"Main".to_string()) && names.contains(&"Inner".to_string()));
    assert!(!names.contains(&"Spare".to_string()), "{names:?}");
}

#[test]
fn preset_effect_keys_start_at_the_playhead() {
    let dir = out_dir("exchange-preset");
    let path = dir.join("blur.ecpreset").to_string_lossy().to_string();
    let mut s = Session::default();
    s.execute("comp.new", json!({"name": "Main", "width": 64, "height": 64, "frameRate": 30, "duration": 10})).unwrap();
    let a = layer_id(s.execute("layer.newSolid", json!({"name": "A", "color": "#ff0000"})).unwrap());
    s.execute("effect.apply", json!({"layer": a, "effect": "Gaussian Blur"})).unwrap();
    s.execute("prop.addKey", json!({"layer": a, "path": "effects/#1/blurriness", "time": 2.0, "value": 0.0})).unwrap();
    s.execute("prop.addKey", json!({"layer": a, "path": "effects/#1/blurriness", "time": 3.0, "value": 20.0})).unwrap();
    s.execute("prop.select", json!({"layer": a, "path": "effects/#1"})).unwrap();
    let r = s.execute("anim.savePreset", json!({"path": path})).unwrap();
    assert_eq!(r["effects"], 1);
    let b = layer_id(s.execute("layer.newSolid", json!({"name": "B", "color": "#00ff00"})).unwrap());
    s.execute("time.set", json!({"time": 5.0})).unwrap();
    s.execute("anim.applyPreset", json!({"path": path, "layers": [b]})).unwrap();
    let c = s.active_comp().unwrap();
    let l = c.layer(effectcraft_project::LayerId(b)).unwrap();
    let keys = l.props.prop("effects/#1/blurriness").unwrap().keys.clone();
    assert_eq!(keys.len(), 2);
    assert!((keys[0].time.seconds() - 5.0).abs() < 1e-6, "{}", keys[0].time.seconds());
    assert!((keys[1].time.seconds() - 6.0).abs() < 1e-6, "{}", keys[1].time.seconds());
}
