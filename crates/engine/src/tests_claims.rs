//! Composition claims (`comp.claim` / `comp.release` / `comp.claims`): another client's edits to
//! a claimed comp are refused; the owner and the user (no `client`) edit as always.

use std::time::Duration;

use effectcraft_project::{ItemId, LayerId};
use serde_json::{Value, json};

use crate::Session;

/// Comps "Main" (with layer L) and "Other"; returns (session, main, other, layer).
fn setup() -> (Session, u64, u64, u64) {
    let mut s = Session::default();
    let other = s.execute("comp.new", json!({"name": "Other", "width": 64, "height": 64})).unwrap()["comp"].as_u64().unwrap();
    let main = s.execute("comp.new", json!({"name": "Main", "width": 64, "height": 64})).unwrap()["comp"].as_u64().unwrap();
    let l = s.execute_checked("layer.newSolid", json!({"name": "L", "color": "#ff0000", "width": 10, "height": 10})).unwrap()["layer"].as_u64().unwrap();
    (s, main, other, l)
}

fn set_opacity(s: &mut Session, main: u64, l: u64, v: f64, client: Option<&str>) -> crate::Result<Value> {
    let mut p = json!({"comp": main, "layer": l, "path": "transform/opacity", "value": v});
    if let Some(c) = client {
        p["client"] = json!(c);
    }
    s.execute_checked("prop.set", p)
}

fn opacity(s: &Session, main: u64, l: u64) -> f64 {
    s.project.comp(ItemId(main)).unwrap().layer(LayerId(l)).unwrap().props.prop("transform/opacity").unwrap().value.as_f64()
}

#[test]
fn another_clients_edit_is_refused_naming_the_owner() {
    let (mut s, main, other, l) = setup();
    let r = s.execute_checked("comp.claim", json!({"comp": "Main", "client": "agentA"})).unwrap();
    assert_eq!(r["comp"], json!(main));
    assert_eq!(r["client"], "agentA");
    assert_eq!(r["ttl"], json!(crate::agents::DEFAULT_TTL));
    let (rev, project) = (s.revision, s.project.clone());
    let e = set_opacity(&mut s, main, l, 10.0, Some("agentB")).unwrap_err().to_string();
    assert!(e.contains("composition \"Main\" is claimed by agentA (expires in"), "{e}");
    assert!(e.contains("work on another comp"), "{e}");
    assert!(std::sync::Arc::ptr_eq(&s.project, &project), "the project is unchanged");
    assert_eq!(s.revision, rev);
    // Deleting the comp or adding a layer to it is refused too; another comp is fine.
    assert!(s.execute_checked("project.delete", json!({"items": [main], "client": "agentB"})).is_err());
    assert!(s.execute_checked("layer.newSolid", json!({"comp": main, "color": "#00ff00", "client": "agentB"})).is_err());
    assert!(s.project.comp(ItemId(main)).is_some());
    s.execute_checked("layer.newSolid", json!({"comp": other, "color": "#00ff00", "client": "agentB"})).unwrap();
    // The owner and the user edit as always.
    set_opacity(&mut s, main, l, 20.0, Some("agentA")).unwrap();
    assert_eq!(opacity(&s, main, l), 20.0);
    set_opacity(&mut s, main, l, 30.0, None).unwrap();
    assert_eq!(opacity(&s, main, l), 30.0);
}

#[test]
fn another_clients_undo_of_a_claimed_comp_is_refused() {
    let (mut s, main, _, l) = setup();
    set_opacity(&mut s, main, l, 40.0, Some("agentB")).unwrap();
    s.execute_checked("comp.claim", json!({"comp": main, "client": "agentA"})).unwrap();
    let e = s.execute_checked("edit.undo", json!({"client": "agentB"})).unwrap_err().to_string();
    assert!(e.contains("claimed by agentA"), "{e}");
    assert_eq!(opacity(&s, main, l), 40.0);
    // After the release it works.
    s.execute_checked("comp.release", json!({"comp": main, "client": "agentA"})).unwrap();
    s.execute_checked("edit.undo", json!({"client": "agentB"})).unwrap();
    assert_eq!(opacity(&s, main, l), 100.0);
}

#[test]
fn release_lets_others_edit() {
    let (mut s, main, _, l) = setup();
    s.execute_checked("comp.claim", json!({"comp": main, "client": "agentA"})).unwrap();
    assert!(set_opacity(&mut s, main, l, 10.0, Some("agentB")).is_err());
    // Only the owner releases it (another client can't).
    assert!(s.execute_checked("comp.release", json!({"comp": main, "client": "agentB"})).is_err());
    let r = s.execute_checked("comp.release", json!({"comp": main, "client": "agentA"})).unwrap();
    assert_eq!(r["released"][0]["comp"], json!(main));
    set_opacity(&mut s, main, l, 10.0, Some("agentB")).unwrap();
    assert_eq!(opacity(&s, main, l), 10.0);
}

#[test]
fn claims_expire() {
    let (mut s, main, _, l) = setup();
    s.execute_checked("comp.claim", json!({"comp": main, "ttl": 60, "client": "agentA"})).unwrap();
    s.claims.clock_offset = Duration::from_secs(30);
    let e = set_opacity(&mut s, main, l, 10.0, Some("agentB")).unwrap_err().to_string();
    assert!(e.contains("expires in 30s"), "{e}");
    s.claims.clock_offset = Duration::from_secs(61);
    set_opacity(&mut s, main, l, 10.0, Some("agentB")).unwrap();
    assert_eq!(s.execute_checked("comp.claims", json!({})).unwrap()["claims"], json!([]));
}

#[test]
fn claim_rules() {
    let (mut s, main, other, _) = setup();
    // A claim needs a client.
    let e = s.execute_checked("comp.claim", json!({"comp": main})).unwrap_err().to_string();
    assert!(e.contains("claims need a `client`"), "{e}");
    s.execute_checked("comp.claim", json!({"comp": main, "client": "agentA"})).unwrap();
    // Taken by another client: refused; by the owner: renewed.
    let e = s.execute_checked("comp.claim", json!({"comp": main, "client": "agentB"})).unwrap_err().to_string();
    assert!(e.contains("already claimed by agentA"), "{e}");
    let r = s.execute_checked("comp.claim", json!({"comp": main, "ttl": 600, "client": "agentA"})).unwrap();
    assert_eq!(r["renewed"], true);
    assert_eq!(r["ttl"], json!(600.0));
    // The longest claim is 24 h.
    let r = s.execute_checked("comp.claim", json!({"comp": other, "ttl": 1e9, "client": "agentB"})).unwrap();
    assert_eq!(r["ttl"], json!(crate::agents::MAX_TTL));
    assert!(r["warnings"][0].as_str().unwrap().contains("24 h"));
    assert!(s.execute_checked("comp.claim", json!({"comp": other, "ttl": -1, "client": "agentB"})).is_err());
    // Listing: comp, name, owner, seconds left. editor.state shows the same.
    let list = s.execute_checked("comp.claims", json!({})).unwrap()["claims"].clone();
    assert_eq!(list.as_array().unwrap().len(), 2);
    let main_claim = list.as_array().unwrap().iter().find(|c| c["comp"] == json!(main)).cloned().unwrap();
    assert_eq!(main_claim["name"], "Main");
    assert_eq!(main_claim["client"], "agentA");
    let left = main_claim["expiresIn"].as_f64().unwrap();
    assert!(left > 590.0 && left <= 600.0, "{left}");
    let owners = |v: &Value| v.as_array().unwrap().iter().map(|c| (c["comp"].clone(), c["client"].clone())).collect::<Vec<_>>();
    assert_eq!(owners(&s.execute_checked("editor.state", json!({})).unwrap()["claims"]), owners(&list));
}

#[test]
fn release_all_and_force() {
    let (mut s, main, other, l) = setup();
    s.execute_checked("comp.claim", json!({"comp": main, "client": "agentA"})).unwrap();
    s.execute_checked("comp.claim", json!({"comp": other, "client": "agentA"})).unwrap();
    // Without `comp`, a client releases all of its own claims.
    let r = s.execute_checked("comp.release", json!({"client": "agentA"})).unwrap();
    assert_eq!(r["released"].as_array().unwrap().len(), 2);
    assert_eq!(s.execute_checked("comp.claims", json!({})).unwrap()["claims"], json!([]));
    // The user releases others' claims only with `force`.
    s.execute_checked("comp.claim", json!({"comp": main, "client": "agentA"})).unwrap();
    let e = s.execute_checked("comp.release", json!({"comp": main})).unwrap_err().to_string();
    assert!(e.contains("force"), "{e}");
    let r = s.execute_checked("comp.release", json!({"comp": main, "force": true})).unwrap();
    assert_eq!(r["released"][0]["client"], "agentA");
    set_opacity(&mut s, main, l, 10.0, Some("agentB")).unwrap();
}

#[test]
fn opening_another_project_drops_claims_and_cursors() {
    let (mut s, main, ..) = setup();
    s.execute_checked("comp.claim", json!({"comp": main, "client": "agentA"})).unwrap();
    s.execute_checked("editor.state", json!({"client": "agentB"})).unwrap();
    // A client opens another project: the user's cursor follows into it (the old one points
    // into the old project).
    s.execute_checked("file.openDemoProject", json!({"client": "agentB"})).unwrap();
    assert!(s.claims.map.is_empty());
    assert_eq!(s.cursors.len(), 1, "only the opener's cursor is left");
    let demo = s.project.items.values().find(|i| i.name == crate::demo::MAIN_COMP).map(|i| i.id);
    assert_eq!(s.active_comp_id(), demo);
    s.replace_project(Default::default(), None);
    assert!(s.cursors.is_empty());
}
