//! Per-layer structural sharing: an edit copies only the layers it changes, so the undo history
//! and the snapshots before and after an edit share every other layer by pointer.

use std::sync::Arc;

use effectcraft_project::{ItemId, LayerSource, Project, build};
use serde_json::json;

use crate::Session;

/// A session with one comp of `n` solid layers (one undo step).
fn big(n: usize) -> (Session, ItemId) {
    let mut s = Session::default();
    let r = s.execute("comp.new", json!({"name": "Big", "width": 64, "height": 64, "duration": 2})).unwrap();
    let cid = ItemId(r["comp"].as_u64().unwrap());
    s.edit("Layers", None, |p, _| {
        let comp = p.comp(cid).unwrap().clone();
        let layers: Vec<_> = (0..n).map(|i| build::layer(p, &comp, &format!("L{i}"), LayerSource::Null, (64, 64), None)).collect();
        p.comp_mut(cid).unwrap().layers.extend(layers);
        Ok(())
    })
    .unwrap();
    (s, cid)
}

fn shared_count(a: &Project, b: &Project, cid: ItemId) -> usize {
    let (a, b) = (&a.comp(cid).unwrap().layers, &b.comp(cid).unwrap().layers);
    (0..a.len()).filter(|&i| a.shares(b, i)).count()
}

#[test]
fn an_edit_to_one_layer_shares_all_the_others() {
    let (mut s, cid) = big(200);
    let before = s.project.clone();
    s.execute("prop.set", json!({"layer": "#57", "path": "transform/opacity", "value": 40})).unwrap();
    let after = s.project.clone();
    assert!(!Arc::ptr_eq(&before, &after));
    assert_eq!(shared_count(&before, &after, cid), 199, "only layer #57 is copied");
    let (b, a) = (&before.comp(cid).unwrap().layers, &after.comp(cid).unwrap().layers);
    assert!(!b.shares(a, 56));
    assert_eq!(a[56].props.prop("transform/opacity").unwrap().value.as_f64(), 40.0);
    assert_eq!(b[56].props.prop("transform/opacity").unwrap().value.as_f64(), 100.0);
    // The history's snapshot is the state before the edit, sharing the same layers.
    let (_, undo_top) = s.history.undo.last().unwrap();
    assert!(Arc::ptr_eq(undo_top, &before));

    // A gesture (merged steps) keeps sharing too.
    for v in [41, 42, 43] {
        s.edit("Opacity", Some("drag"), |p, _| {
            let l = p.comp_mut(cid).unwrap().layers.get_mut(10).unwrap();
            l.props.prop_mut("transform/opacity").unwrap().value = effectcraft_keyframe::Value::Scalar(v as f64);
            Ok(())
        })
        .unwrap();
    }
    assert_eq!(shared_count(&after, &s.project, cid), 199);

    // Undo and redo restore exactly.
    let edited = s.project.clone();
    assert!(s.undo());
    assert!(s.project.same_content(&after) && *s.project == *after);
    assert!(s.undo());
    assert!(*s.project == *before);
    assert_eq!(s.project.comp(cid).unwrap().layers[56].props.prop("transform/opacity").unwrap().value.as_f64(), 100.0);
    assert!(s.redo() && s.redo());
    assert!(*s.project == *edited);
    assert_eq!(serde_json::to_string(&*s.project).unwrap(), serde_json::to_string(&*edited).unwrap());
}

#[test]
fn a_no_op_edit_makes_no_undo_step() {
    let (mut s, cid) = big(50);
    let steps = s.history.undo.len();
    let before = s.project.clone();
    // Touches every layer mutably but changes nothing: compared layer by layer, no new step.
    s.edit("Nothing", None, |p, _| {
        for l in p.comp_mut(cid).unwrap().layers.iter_mut() {
            l.name = l.name.clone();
        }
        Ok(())
    })
    .unwrap();
    assert_eq!(s.history.undo.len(), steps);
    assert!(Arc::ptr_eq(&s.project, &before));
}

#[test]
fn deleting_a_layer_copies_only_the_layers_that_referred_to_it() {
    let (mut s, cid) = big(20);
    // Layer #3 is parented to #2.
    let parent = s.project.comp(cid).unwrap().layers[1].id;
    s.edit("Parent", None, |p, _| {
        p.comp_mut(cid).unwrap().layers[2].parent = Some(parent);
        Ok(())
    })
    .unwrap();
    let before = s.project.clone();
    s.execute("edit.clear", json!({"layers": [parent.0]})).unwrap();
    let after = &s.project.comp(cid).unwrap().layers;
    let old = before.comp(cid).unwrap().layers.arcs();
    assert_eq!(after.len(), 19);
    let reused = after.arcs().iter().filter(|a| old.iter().any(|b| Arc::ptr_eq(a, b))).count();
    assert_eq!(reused, 18, "only the child (unparented) is copied");
    assert_eq!(after[1].parent, None);
}
