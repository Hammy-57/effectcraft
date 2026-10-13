//! Per-agent cursors: a request with `client` works on that client's own selection, active comp
//! and current time; the user's (no `client`) stay as they were.

use effectcraft_project::{ItemId, LayerId};
use serde_json::{Value, json};

use crate::{Event, Session};

fn solid(s: &mut Session, name: &str) -> u64 {
    s.execute_checked("layer.newSolid", json!({"name": name, "color": "#ff0000", "width": 10, "height": 10})).unwrap()["layer"].as_u64().unwrap()
}

fn opacity(s: &Session, layer: u64) -> f64 {
    s.active_comp().unwrap().layer(LayerId(layer)).unwrap().props.prop("transform/opacity").unwrap().value.as_f64()
}

/// A comp with layers X, Y and Z; the user has Z selected.
fn setup() -> (Session, u64, u64, u64) {
    let mut s = Session::default();
    s.execute("comp.new", json!({"name": "Main", "width": 64, "height": 64, "duration": 4})).unwrap();
    let (x, y, z) = (solid(&mut s, "X"), solid(&mut s, "Y"), solid(&mut s, "Z"));
    s.execute_checked("layer.select", json!({"layers": [z]})).unwrap();
    (s, x, y, z)
}

#[test]
fn each_client_edits_its_own_selection() {
    let (mut s, x, y, z) = setup();
    s.execute_checked("layer.select", json!({"layers": [x], "client": "A"})).unwrap();
    s.execute_checked("layer.select", json!({"layers": [y], "client": "B"})).unwrap();
    // The selected-layer edits (no `layer`) land on each client's own selection.
    s.execute_checked("prop.set", json!({"path": "transform/opacity", "value": 10, "client": "A"})).unwrap();
    s.execute_checked("prop.set", json!({"path": "transform/opacity", "value": 20, "client": "B"})).unwrap();
    assert_eq!(opacity(&s, x), 10.0);
    assert_eq!(opacity(&s, y), 20.0);
    assert_eq!(opacity(&s, z), 100.0);
    // The user's selection didn't move, and the user's own edit still goes to it.
    assert_eq!(s.state.selected_layers, vec![LayerId(z)]);
    s.execute_checked("prop.set", json!({"path": "transform/opacity", "value": 30})).unwrap();
    assert_eq!(opacity(&s, z), 30.0);
    assert_eq!(opacity(&s, x), 10.0);
    // editor.state with a client is that client's view; without, the user's (plus the clients).
    let a = s.execute_checked("editor.state", json!({"client": "A"})).unwrap();
    assert_eq!(a["selected_layers"], json!([x]));
    assert_eq!(a["client"], "A");
    let me = s.execute_checked("editor.state", json!({})).unwrap();
    assert_eq!(me["selected_layers"], json!([z]));
    assert!(me.get("client").is_none());
    let clients = me["clients"].as_array().unwrap();
    let find = |n: &str| clients.iter().find(|c| c["client"] == n).cloned().unwrap();
    assert_eq!(find("A")["selected_layers"], json!([x]));
    assert_eq!(find("B")["selected_layers"], json!([y]));
    assert!(s.client.is_none(), "the client is only set while its command runs");
}

#[test]
fn a_new_client_starts_from_the_users_cursor() {
    let (mut s, _, _, z) = setup();
    // No selection of its own yet: it sees the user's (a copy).
    s.execute_checked("prop.set", json!({"path": "transform/opacity", "value": 50, "client": "C"})).unwrap();
    assert_eq!(opacity(&s, z), 50.0);
    // Its later changes are its own.
    s.execute_checked("layer.select", json!({"layers": [], "client": "C"})).unwrap();
    assert_eq!(s.state.selected_layers, vec![LayerId(z)]);
}

#[test]
fn client_time_is_its_own() {
    let (mut s, ..) = setup();
    s.execute_checked("time.set", json!({"time": 1.0})).unwrap();
    let human = s.time();
    s.execute_checked("time.set", json!({"time": 2.5, "client": "A"})).unwrap();
    assert_eq!(s.time(), human, "the user's current time stays");
    let client_time = |s: &mut Session| s.execute_checked("editor.state", json!({})).unwrap()["clients"][0]["time"].as_f64().unwrap();
    // (snapped to the comp's 29.97 fps frames)
    assert!((client_time(&mut s) - 2.5).abs() < 0.05);
    // The user moving time doesn't move the client's.
    s.execute_checked("time.set", json!({"time": 0.5})).unwrap();
    assert!((client_time(&mut s) - 2.5).abs() < 0.05);
    assert!((s.time().seconds() - 0.5).abs() < 0.05);
}

#[test]
fn a_client_creating_a_comp_keeps_the_users_view() {
    let (mut s, ..) = setup();
    let main = s.active_comp_id().unwrap();
    s.drain_events();
    let new = s.execute_checked("comp.new", json!({"name": "Agent's", "width": 32, "height": 32, "client": "A"})).unwrap()["comp"].as_u64().unwrap();
    assert_eq!(s.active_comp_id(), Some(main), "the user's active comp stays");
    assert!(s.state.open_comps.contains(&ItemId(new)), "the comp gets a tab");
    assert!(!s.drain_events().iter().any(|e| matches!(e, Event::OpenComp(_))), "the user's viewer isn't switched");
    // The client works in its new comp without naming it.
    let l = s.execute_checked("layer.newSolid", json!({"color": "#00ff00", "client": "A"})).unwrap()["layer"].as_u64().unwrap();
    assert!(s.project.comp(ItemId(new)).unwrap().layer(LayerId(l)).is_some());
    assert_eq!(s.project.comp(main).unwrap().layers.len(), 3);
    let a: Value = s.execute_checked("editor.state", json!({"client": "A"})).unwrap();
    assert_eq!(a["active_comp"], json!(new));
}

#[test]
fn parked_comp_selections_and_recent_comps_are_per_client() {
    let (mut s, x, _, z) = setup();
    let main = s.active_comp_id().unwrap();
    let other = ItemId(s.execute("comp.new", json!({"name": "Other"})).unwrap()["comp"].as_u64().unwrap());
    let other_layer = solid(&mut s, "Other layer");
    s.open_comp(main);
    let parked = s.state.comp_selections.clone();
    let recent = s.state.recent_comps.clone();
    s.execute_checked("layer.select", json!({"layers": [x], "client": "A"})).unwrap();
    s.execute_checked("comp.open", json!({"comp": other.0, "client": "A"})).unwrap();
    s.execute_checked("layer.select", json!({"layers": [], "client": "A"})).unwrap();
    s.execute_checked("comp.open", json!({"comp": main.0, "client": "A"})).unwrap();
    let agent = s.execute_checked("editor.state", json!({"client": "A"})).unwrap();
    assert_eq!(agent["selected_layers"], json!([x]));
    assert_eq!(s.state.selected_layers, vec![LayerId(z)]);
    assert_eq!(s.state.comp_selections, parked);
    assert_eq!(s.state.recent_comps, recent);
    s.open_comp(other);
    assert_eq!(s.state.selected_layers, vec![LayerId(other_layer)]);
}

#[test]
fn without_client_nothing_changes() {
    let (mut s, x, ..) = setup();
    let main = s.active_comp_id().unwrap();
    s.execute_checked("layer.select", json!({"layers": [x]})).unwrap();
    assert_eq!(s.state.selected_layers, vec![LayerId(x)]);
    let new = s.execute_checked("comp.new", json!({"name": "Mine", "width": 32, "height": 32})).unwrap()["comp"].as_u64().unwrap();
    assert_eq!(s.active_comp_id(), Some(ItemId(new)));
    assert!(s.drain_events().iter().any(|e| matches!(e, Event::OpenComp(c) if *c == ItemId(new))));
    assert_ne!(s.active_comp_id(), Some(main));
    assert!(s.cursors.is_empty(), "no client cursors without clients");
}

#[test]
fn a_client_deleting_the_users_layer_sanitizes_the_users_selection() {
    let (mut s, _, _, z) = setup();
    s.execute_checked("edit.clear", json!({"layers": [z], "client": "A"})).unwrap();
    assert!(s.state.selected_layers.is_empty());
}

#[test]
fn a_client_error_restores_the_users_cursor() {
    let (mut s, x, _, z) = setup();
    s.execute_checked("layer.select", json!({"layers": [x], "client": "A"})).unwrap();
    assert!(s.execute_checked("prop.set", json!({"path": "transform/nope", "value": 1, "client": "A"})).is_err());
    assert_eq!(s.state.selected_layers, vec![LayerId(z)]);
    let a = s.execute_checked("editor.state", json!({"client": "A"})).unwrap();
    assert_eq!(a["selected_layers"], json!([x]));
}

#[test]
fn batch_steps_use_the_batch_clients_cursor() {
    let (mut s, x, _, z) = setup();
    s.execute_checked(
        "engine.batch",
        json!({"client": "A", "steps": [
            {"command": "layer.select", "params": {"layers": [x]}},
            {"command": "prop.set", "params": {"path": "transform/opacity", "value": 5}},
        ]}),
    )
    .unwrap();
    assert_eq!(opacity(&s, x), 5.0);
    assert_eq!(s.state.selected_layers, vec![LayerId(z)]);
}

#[test]
fn cursors_are_capped() {
    let (mut s, ..) = setup();
    for i in 0..(crate::agents::MAX_CURSORS + 10) {
        s.execute_checked("editor.state", json!({"client": format!("agent-{i}")})).unwrap();
    }
    assert_eq!(s.cursors.len(), crate::agents::MAX_CURSORS);
    // The least recently used went first.
    assert!(s.cursors.get("agent-0").is_none());
    assert!(s.cursors.get(&format!("agent-{}", crate::agents::MAX_CURSORS + 9)).is_some());
    let long = "x".repeat(crate::agents::MAX_CLIENT_LEN + 1);
    assert!(s.execute_checked("editor.state", json!({"client": long})).is_err());
}
