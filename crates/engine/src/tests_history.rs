//! Branching history: undo then a new step keeps the undone line as a branch; the tree lists
//! every state; jumping to any state (on any branch) restores it and keeps everything else.

use serde_json::{Value, json};

use crate::Session;

fn names(s: &Session) -> Vec<String> {
    s.project.comps().next().map(|(_, c)| c.layers.iter().map(|l| l.name.clone()).collect()).unwrap_or_default()
}

fn solid(s: &mut Session, name: &str) {
    s.execute("layer.newSolid", json!({"name": name, "color": "#ff0000", "width": 10, "height": 10})).unwrap();
}

fn list(s: &mut Session) -> Vec<Value> {
    s.execute("edit.history.list", json!({})).unwrap()["states"].as_array().cloned().unwrap()
}

fn index_of(states: &[Value], label: &str, nth: usize) -> u64 {
    states.iter().filter(|n| n["label"] == label).nth(nth).unwrap_or_else(|| panic!("no state {label} in {states:?}"))["index"].as_u64().unwrap()
}

#[test]
fn undo_then_new_step_keeps_a_branch() {
    let mut s = Session::default();
    s.execute("comp.new", json!({"name": "H", "width": 64, "height": 64, "duration": 2})).unwrap();
    solid(&mut s, "A");
    solid(&mut s, "B");
    solid(&mut s, "C");
    // Linear so far: Original, New Composition, 3 solids.
    let st = list(&mut s);
    assert_eq!(st.len(), 5, "{st:?}");
    assert!(st.iter().all(|n| n["depth"] == 0));
    assert_eq!(st.last().unwrap()["current"], true);
    // Undo twice (B and C undone), then a new step: AE would forget B and C.
    assert!(s.undo() && s.undo());
    let st = list(&mut s);
    assert_eq!(st.iter().filter(|n| n["future"] == true).count(), 2);
    solid(&mut s, "D");
    assert!(s.history.redo.is_empty(), "the new step clears Redo, as in After Effects");
    assert_eq!(s.history.branches.len(), 1);
    assert_eq!(names(&s), ["D", "A"]);
    let st = list(&mut s);
    assert_eq!(st.len(), 6, "the undone states are still listed: {st:?}");
    let branch: Vec<&Value> = st.iter().filter(|n| n["depth"] == 1).collect();
    assert_eq!(branch.len(), 2, "B and C sit on a branch: {st:?}");
    // Jump to C on the old branch.
    let c = st.iter().rfind(|n| n["depth"] == 1).unwrap()["index"].as_u64().unwrap();
    let r = s.execute("edit.history.goto", json!({"index": c})).unwrap();
    assert_eq!(r["label"], "New Solid", "{r}");
    assert_eq!(names(&s), ["C", "B", "A"]);
    // Now D is the branch, and Undo walks the B/C line.
    assert_eq!(s.history.branches.len(), 1);
    assert_eq!(s.history.branches[0].steps.len(), 1);
    assert!(s.undo());
    assert_eq!(names(&s), ["B", "A"]);
    assert!(s.redo());
    // ...and back to D by id.
    let st = list(&mut s);
    let d = st.iter().find(|n| n["depth"] == 1).unwrap()["id"].as_str().unwrap().to_string();
    s.execute("edit.history.goto", json!({"id": d})).unwrap();
    assert_eq!(names(&s), ["D", "A"]);
    let st = list(&mut s);
    assert_eq!(st.len(), 6);
    assert_eq!(st.iter().filter(|n| n["current"] == true).count(), 1);
    // The original state.
    let root = index_of(&st, "Original", 0);
    s.execute("edit.history.goto", json!({"index": root})).unwrap();
    assert!(s.project.comps().next().is_none());
    assert!(s.history.undo.is_empty());
    assert_eq!(list(&mut s).len(), 6, "nothing is lost by jumping");
    // Relative steps along the line.
    s.execute("edit.history.goto", json!({"steps": 2})).unwrap();
    assert_eq!(names(&s), ["A"]);
    assert!(s.execute("edit.history.goto", json!({"id": "nope"})).is_err());
    assert!(s.execute("edit.history.goto", json!({})).is_err());
}

#[test]
fn branches_of_branches_and_purge() {
    let mut s = Session::default();
    s.execute("comp.new", json!({"name": "H", "width": 64, "height": 64, "duration": 2})).unwrap();
    solid(&mut s, "A");
    solid(&mut s, "B");
    s.undo();
    solid(&mut s, "C"); // branch 1: B
    s.undo();
    solid(&mut s, "D"); // branch 2: C
    let st = list(&mut s);
    assert_eq!(st.len(), 6, "{st:?}");
    // Go to B, undo to A, make E: D's line becomes a branch, and a branch of the new line.
    let b = st.iter().find(|n| n["depth"] == 1 && n["label"] == "New Solid").unwrap()["index"].as_u64().unwrap();
    s.execute("edit.history.goto", json!({"index": b})).unwrap();
    let which = names(&s);
    assert!(which == ["B", "A"] || which == ["C", "A"], "{which:?}");
    s.undo();
    solid(&mut s, "E");
    let st = list(&mut s);
    assert_eq!(st.len(), 7, "{st:?}");
    // Every state is reachable and restores its own layers.
    let mut seen = std::collections::BTreeSet::new();
    for i in 0..st.len() {
        s.execute("edit.history.goto", json!({"index": i})).unwrap();
        seen.insert(names(&s).join(","));
        assert_eq!(list(&mut s).len(), 7);
    }
    for want in ["", "A", "B,A", "C,A", "D,A", "E,A"] {
        assert!(seen.contains(want), "{want:?} not reachable: {seen:?}");
    }
    s.execute("edit.purgeUndo", json!({})).unwrap();
    assert_eq!(list(&mut s).len(), 1);
}

#[test]
fn undo_levels_trim_branches() {
    let mut s = Session::default();
    s.prefs.general.undo_levels = 3;
    s.execute("comp.new", json!({"name": "H", "width": 64, "height": 64, "duration": 2})).unwrap();
    for n in ["A", "B", "C", "D", "E"] {
        solid(&mut s, n);
    }
    assert_eq!(s.history.undo.len(), 3);
    s.undo();
    s.undo();
    solid(&mut s, "F");
    // The branch (D, E) grew from a state still kept.
    assert_eq!(s.history.branch_states(), 2);
    for n in ["G", "H", "I"] {
        solid(&mut s, n);
    }
    // Its parent fell off the end of the undo levels: the branch goes too.
    assert_eq!(s.history.branch_states(), 0);
    assert_eq!(list(&mut s).len(), 4);
}

#[test]
fn modified_mark_follows_undo_and_no_op_edits_record_nothing() {
    let mut s = Session::default();
    s.execute("comp.new", json!({"name": "H", "width": 64, "height": 64, "duration": 2})).unwrap();
    solid(&mut s, "A");
    let path = std::env::temp_dir().join(format!("ec-dirty-{}.ecproj", std::process::id()));
    s.execute("file.saveAs", json!({"path": path.to_string_lossy()})).unwrap();
    assert!(!s.is_dirty());
    // Setting a value to what it already is: no undo step, still saved.
    let steps = s.history.undo.len();
    s.execute("prop.set", json!({"layer": "A", "path": "transform/opacity", "value": 100})).unwrap();
    assert_eq!(s.history.undo.len(), steps);
    assert!(!s.is_dirty());
    // A real change, then undo back to the saved state: not modified; redo: modified again.
    s.execute("prop.set", json!({"layer": "A", "path": "transform/opacity", "value": 50})).unwrap();
    assert!(s.is_dirty());
    s.execute("edit.undo", json!({})).unwrap();
    assert!(!s.is_dirty(), "undo back to the saved state");
    s.execute("edit.redo", json!({})).unwrap();
    assert!(s.is_dirty());
    // History jumps: the saved state is the parent of the current (changed) one.
    let states = list(&mut s);
    let cur = states.iter().find(|n| n["current"] == true).unwrap();
    let (changed, saved) = (cur["index"].as_u64().unwrap(), cur["parent"].as_u64().unwrap());
    s.execute("edit.history.goto", json!({"index": saved})).unwrap();
    assert!(!s.is_dirty(), "jumped to the saved state");
    s.execute("edit.history.goto", json!({"index": changed})).unwrap();
    assert!(s.is_dirty());
    s.execute("edit.undo", json!({})).unwrap();
    assert!(!s.is_dirty());
    let r = s.execute(
        "engine.batch",
        json!({"atomic": true, "steps": [
            {"command": "prop.set", "params": {"layer": "A", "path": "transform/opacity", "value": 10}},
            {"command": "no.suchCommand", "params": {}}
        ]}),
    );
    assert!(r.is_err());
    assert!(!s.is_dirty(), "rolled back");
    let _ = std::fs::remove_file(path);
}

#[test]
fn a_session_restored_with_unsaved_changes_stays_modified() {
    let mut s = Session::default();
    s.execute("comp.new", json!({"name": "H", "width": 64, "height": 64, "duration": 2})).unwrap();
    let p = (*s.project).clone();
    // The web app reopens the last visit's project, which had unsaved changes.
    s.replace_project(p, None);
    assert!(!s.is_dirty());
    s.mark_unsaved();
    assert!(s.is_dirty());
    solid(&mut s, "A");
    s.execute("edit.undo", json!({})).unwrap();
    assert!(s.is_dirty(), "undo doesn't reach a saved state");
}

// ---------------------------------------------------------------- agents and the user together

fn new_solid(s: &mut Session, name: &str, client: Option<&str>) -> u64 {
    let mut p = json!({"name": name, "color": "#ff0000", "width": 10, "height": 10});
    if let Some(c) = client {
        p["client"] = json!(c);
    }
    s.execute_checked("layer.newSolid", p).unwrap()["layer"].as_u64().unwrap()
}

#[test]
fn a_number_is_a_layer_id_never_a_stack_position() {
    let mut s = Session::default();
    s.execute("comp.new", json!({"name": "Ids", "width": 64, "height": 64})).unwrap();
    let red = new_solid(&mut s, "Red", None);
    let blue = new_solid(&mut s, "Blue", None);
    // A number that is no layer's id is an error naming it, not the layer at that position.
    let missing = (1..).find(|n| *n != red && *n != blue).unwrap();
    let e = s.execute_checked("layer.rename", json!({"layer": missing, "name": "Oops"})).unwrap_err().to_string();
    assert!(e.contains(&format!("no layer {missing}")) && e.contains("\"#n\""), "{e}");
    assert_eq!(names(&s), ["Blue", "Red"]);
    // "#n" is the stack position; an id stays the same layer after a reorder.
    s.execute_checked("layer.rename", json!({"layer": "#2", "name": "Red 2"})).unwrap();
    s.execute_checked("layer.arrange", json!({"layers": [red], "to": "front"})).unwrap();
    s.execute_checked("layer.rename", json!({"layer": red, "name": "Red 3"})).unwrap();
    assert_eq!(names(&s), ["Red 3", "Blue"]);
    // A deleted layer's id hits nothing (not the layer now at that position).
    s.execute_checked("edit.clear", json!({"layers": [blue]})).unwrap();
    assert!(s.execute_checked("layer.rename", json!({"layer": blue, "name": "Stale"})).is_err());
    assert_eq!(names(&s), ["Red 3"]);
}

#[test]
fn ids_are_not_reused_after_undo() {
    let mut s = Session::default();
    s.execute("comp.new", json!({"name": "Ids", "width": 64, "height": 64})).unwrap();
    let a = new_solid(&mut s, "Agent", Some("agent"));
    s.execute("edit.undo", json!({})).unwrap();
    let b = new_solid(&mut s, "User", None);
    assert_ne!(a, b, "an undone layer's id is never handed to another layer");
    s.execute("edit.undo", json!({})).unwrap();
    s.execute("edit.redo", json!({})).unwrap();
    assert!(s.active_comp().unwrap().layer(effectcraft_project::LayerId(b)).is_some());
}

#[test]
fn a_client_undoes_only_its_own_last_step() {
    let mut s = Session::default();
    s.execute("comp.new", json!({"name": "U", "width": 64, "height": 64})).unwrap();
    new_solid(&mut s, "Agent's", Some("agent-1"));
    new_solid(&mut s, "User's", None);
    let rev = s.revision;
    let e = s.execute_checked("edit.undo", json!({"client": "agent-1"})).unwrap_err().to_string();
    assert!(e.contains("made by the user") && e.contains("undo without `client`"), "{e}");
    assert_eq!(s.revision, rev, "a refused undo changes nothing");
    assert_eq!(names(&s), ["User's", "Agent's"]);
    // Another client's step is refused too; the user's undo (no client) works as always.
    new_solid(&mut s, "Other's", Some("agent-2"));
    let e = s.execute_checked("edit.undo", json!({"client": "agent-1"})).unwrap_err().to_string();
    assert!(e.contains("client `agent-2`"), "{e}");
    s.execute_checked("edit.undo", json!({"client": "agent-2"})).unwrap();
    s.execute("edit.undo", json!({})).unwrap();
    s.execute_checked("edit.undo", json!({"client": "agent-1"})).unwrap();
    assert!(names(&s).is_empty());
    // The History list names who made each state; batch steps inherit the batch's client.
    s.execute_checked("engine.batch", json!({"client": "agent-3", "steps": [{"command": "layer.newSolid", "params": {"color": "#00ff00"}}]})).unwrap();
    assert_eq!(list(&mut s).iter().find(|n| n["current"] == true).unwrap()["client"], "agent-3");
    s.execute_checked("edit.undo", json!({"client": "agent-3"})).unwrap();
    assert!(s.client.is_none(), "the client is only set while its command runs");
}

#[test]
fn expect_revision_refuses_a_changed_document() {
    let mut s = Session::default();
    s.execute("comp.new", json!({"name": "R", "width": 64, "height": 64})).unwrap();
    let rev = s.execute_checked("project.summary", json!({})).unwrap()["revision"].as_u64().unwrap();
    assert_eq!(s.execute_checked("editor.state", json!({})).unwrap()["revision"], rev);
    new_solid(&mut s, "Meanwhile", None);
    let e = s.execute_checked("layer.newSolid", json!({"color": "#0000ff", "expectRevision": rev})).unwrap_err().to_string();
    assert!(e.contains(&format!("document changed (revision {}, expected {rev})", s.revision)), "{e}");
    assert_eq!(names(&s), ["Meanwhile"], "nothing ran");
    let now = s.revision;
    s.execute_checked("layer.newSolid", json!({"color": "#0000ff", "expectRevision": now, "client": "agent"})).unwrap();
    assert_eq!(names(&s).len(), 2);
}
