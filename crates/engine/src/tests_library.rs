//! Libraries: `library.json` folders searched and inserted from (comps, presets, media).

use effectcraft_project::{ItemId, LayerSource};
use serde_json::json;

use crate::Session;

fn out_dir(name: &str) -> std::path::PathBuf {
    let d = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/test-out").join(name);
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A library folder: a project holding "Intro" and its nested "Badge", plus index entries for
/// it, a preset, a sound and an entry whose file isn't there.
fn library(dir: &std::path::Path) {
    let mut s = Session::default();
    s.execute("comp.new", json!({"name": "Badge", "width": 32, "height": 32, "frameRate": 30, "duration": 1})).unwrap();
    let badge = s.active_comp_id().unwrap();
    s.execute("layer.newSolid", json!({"name": "Dot", "color": "#ff0000"})).unwrap();
    s.execute("comp.new", json!({"name": "Intro", "width": 64, "height": 64, "frameRate": 30, "duration": 2})).unwrap();
    let intro = s.active_comp_id().unwrap();
    s.execute("layer.addItem", json!({"comp": intro.0, "item": badge.0, "time": 0.0})).unwrap();
    std::fs::create_dir_all(dir.join("projects")).unwrap();
    s.execute("file.saveAs", json!({"path": dir.join("projects/intro.ecproj").to_string_lossy()})).unwrap();
    let index = json!({
        "format": "effectcraft-library",
        "version": 1,
        "name": "Studio",
        "entries": [
            {"id": "intro", "name": "Logo Intro", "kind": "comp", "file": "projects/intro.ecproj", "comp": "Intro",
             "category": "Logos", "tags": ["sting", "reveal"], "description": "A badge pops in", "duration": 2.0},
            {"id": "badge", "name": "Badge Pop", "kind": "comp", "file": "projects/intro.ecproj", "comp": "Badge",
             "category": "Logos", "tags": ["dot"], "description": "Just the badge"},
            {"id": "whoosh", "name": "Fast Whoosh", "kind": "audio", "file": "sfx/whoosh.wav", "category": "SFX/Transitions",
             "tags": ["swoosh"], "description": "Quick air pass for an intro"},
            {"id": "gone", "name": "Lost Title", "kind": "comp", "file": "projects/gone.ecproj", "category": "Titles"},
            {"id": "", "name": "No id is skipped", "kind": "comp"}
        ]
    });
    std::fs::write(dir.join("library.json"), serde_json::to_vec(&index).unwrap()).unwrap();
}

#[test]
fn library_search_ranks_and_filters() {
    let dir = out_dir("library-search");
    library(&dir);
    let mut s = Session::default();
    let r = s.execute("library.addFolder", json!({"path": dir.to_string_lossy()})).unwrap();
    assert_eq!((r["name"].as_str(), r["entries"].as_u64()), (Some("Studio"), Some(4)));

    // Name hits outrank description hits; every word must match.
    let r = s.execute("library.search", json!({"query": "intro"})).unwrap();
    let ids: Vec<&str> = r["entries"].as_array().unwrap().iter().map(|e| e["id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["intro", "whoosh"]);
    let r = s.execute("library.search", json!({"query": "logo reveal"})).unwrap();
    assert_eq!(r["total"], 1);
    assert_eq!(r["entries"][0]["library"], "Studio");
    // Prefixes match; kind and category filter; no query lists everything by name.
    assert_eq!(s.execute("library.search", json!({"query": "swoo"})).unwrap()["entries"][0]["id"], "whoosh");
    assert_eq!(s.execute("library.search", json!({"kind": "audio"})).unwrap()["total"], 1);
    assert_eq!(s.execute("library.search", json!({"category": "logos"})).unwrap()["total"], 2);
    let all = s.execute("library.search", json!({"limit": 2, "offset": 1})).unwrap();
    assert_eq!(all["total"], 4);
    assert_eq!(all["entries"].as_array().unwrap().len(), 2);
    assert_eq!(s.execute("library.search", json!({"query": "zebra"})).unwrap()["total"], 0);

    // get resolves paths against the folder.
    let e = s.execute("library.get", json!({"id": "intro"})).unwrap();
    assert_eq!(std::path::Path::new(e["file"].as_str().unwrap()), dir.join("projects/intro.ecproj"));
    assert!(s.execute("library.get", json!({"id": "nope"})).is_err());

    // A broken folder is reported, and the others keep working.
    let broken = out_dir("library-broken");
    std::fs::write(broken.join("library.json"), b"{nope").unwrap();
    assert!(s.execute("library.addFolder", json!({"path": broken.to_string_lossy()})).is_err());
    s.prefs.libraries.push(broken.to_string_lossy().to_string());
    let f = s.execute("library.folders", json!({})).unwrap();
    assert!(f["libraries"][1]["error"].is_string());
    assert_eq!(s.execute("library.search", json!({"query": "badge"})).unwrap()["total"], 2);
    assert_eq!(s.execute("library.removeFolder", json!({"path": broken.to_string_lossy()})).unwrap()["removed"], true);
    assert_eq!(s.prefs.libraries.len(), 1);
}

#[test]
fn library_insert_nests_the_named_comp() {
    let dir = out_dir("library-insert");
    library(&dir);
    let mut s = Session::default();
    s.prefs.libraries.push(dir.to_string_lossy().to_string());
    s.execute("comp.new", json!({"name": "Show", "width": 64, "height": 64, "frameRate": 30, "duration": 4})).unwrap();
    let show = s.active_comp_id().unwrap();

    // The entry's own comp ("Badge") is nested, not the project's root comp ("Intro").
    let r = s.execute("library.insert", json!({"id": "badge", "time": 1.0})).unwrap();
    let item = ItemId(r["item"].as_u64().unwrap());
    assert_eq!(s.project.item(item).unwrap().name, "Badge");
    let layer = s.project.comp(show).unwrap().layers.iter().find(|l| l.id.0 == r["layer"].as_u64().unwrap()).unwrap().clone();
    assert!(matches!(layer.source, LayerSource::Comp { item: i } if i == item));
    assert_eq!(r["import"]["primaryComp"].as_u64().map(|_| ()), Some(()));

    let r = s.execute("library.insert", json!({"id": "intro"})).unwrap();
    assert_eq!(s.project.item(ItemId(r["item"].as_u64().unwrap())).unwrap().name, "Intro");
    assert_eq!(s.project.comp(show).unwrap().layers.len(), 2);

    // Undo removes the layer, then the import.
    let items = s.project.items.len();
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.project.comp(show).unwrap().layers.len(), 1);
    s.execute("edit.undo", json!({})).unwrap();
    assert!(s.project.items.len() < items);

    // Missing files and kinds it can't insert are errors that change nothing.
    let before = s.project.items.len();
    let e = s.execute("library.insert", json!({"id": "gone"})).unwrap_err();
    assert!(e.to_string().contains("Lost Title"), "{e}");
    assert!(s.execute("library.insert", json!({"id": "whoosh"})).is_err());
    assert_eq!(s.project.items.len(), before);
}
