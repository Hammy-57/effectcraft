//! Heavy-layer suggestions for Smart Freeze: synthetic render times, no rendering needed.

use effectcraft_project::{ItemId, LayerId};
use serde_json::json;

use crate::Session;

struct Fixture {
    s: Session,
    outer: ItemId,
    inner: ItemId,
    stack: LayerId,
    solid: LayerId,
    inside: LayerId,
}

/// "Outer" (10 fps, so a 100 ms budget) holds a cheap solid and the precomp layer "Stat Stack"
/// of "Inner", which holds one layer of its own.
fn fixture() -> Fixture {
    let mut s = Session::default();
    s.execute("comp.new", json!({"name":"Inner", "width":16, "height":16, "frameRate":10, "duration":0.2})).unwrap();
    let inner = s.active_comp_id().unwrap();
    s.execute("layer.newSolid", json!({"name":"Inside", "color":"#00ff00", "width":8, "height":8})).unwrap();
    let inside = s.active_comp().unwrap().layers[0].id;
    s.execute("comp.new", json!({"name":"Outer", "width":16, "height":16, "frameRate":10, "duration":0.2})).unwrap();
    let outer = s.active_comp_id().unwrap();
    s.execute("layer.newSolid", json!({"name":"Red", "color":"#ff0000", "width":8, "height":8})).unwrap();
    let solid = s.active_comp().unwrap().layers[0].id;
    s.execute("layer.addItem", json!({"item":inner.0})).unwrap();
    let stack = s.active_comp().unwrap().layers[0].id;
    s.execute("layer.rename", json!({"layer":stack.0, "name":"Stat Stack"})).ok();
    Fixture { s, outer, inner, stack, solid, inside }
}

fn play(f: &mut Fixture, frames: usize, stack_ms: f64, solid_ms: f64, inside_ms: f64) {
    for _ in 0..frames {
        f.s.note_layer_costs(f.outer, [(f.outer, f.stack, stack_ms), (f.outer, f.solid, solid_ms), (f.inner, f.inside, inside_ms)]);
    }
}

#[test]
fn a_heavy_precomp_is_suggested_and_a_cheap_layer_is_not() {
    let mut f = fixture();
    play(&mut f, 12, 1800.0, 2.0, 1700.0);
    let found = f.s.freeze_suggestions();
    assert_eq!(found.len(), 1, "the solid is cheap and the layer inside is covered by freezing its precomp: {found:?}");
    let top = &found[0];
    assert_eq!((top.comp, top.layer, top.precomp), (f.outer, f.stack, Some(f.inner)));
    assert!((top.ratio() - 18.0).abs() < 1e-6, "1800 ms against a 100 ms budget");
    assert!(top.hint().contains("1.8 s/frame"), "{}", top.hint());
    let listed = f.s.execute("freeze.suggest", json!({})).unwrap();
    assert_eq!(listed["candidates"][0]["layer"], f.stack.0);
    assert_eq!(listed["candidates"][0]["freeze"]["mode"], "precomp");
    assert_eq!(listed["autoHeavy"], false);
    assert!(f.s.project.freeze.sections.is_empty(), "suggesting never freezes");
}

#[test]
fn only_a_whole_section_of_slow_frames_counts() {
    let mut f = fixture();
    play(&mut f, 7, 1800.0, 2.0, 2.0);
    assert!(f.s.freeze_suggestions().is_empty(), "too few frames measured");
    play(&mut f, 1, 1800.0, 2.0, 2.0);
    assert_eq!(f.s.freeze_suggestions().len(), 1);
    play(&mut f, 16, 40.0, 2.0, 2.0);
    assert!(f.s.freeze_suggestions().is_empty(), "back under the budget once the section has passed");
    let mut g = fixture();
    for i in 0..20 {
        play(&mut g, 1, if i % 5 == 0 { 1800.0 } else { 30.0 }, 2.0, 2.0);
    }
    assert!(g.s.freeze_suggestions().is_empty(), "occasional slow frames are not a heavy section");
}

#[test]
fn a_heavy_layer_inside_the_comp_is_suggested_and_bad_times_are_ignored() {
    let mut f = fixture();
    play(&mut f, 12, 5.0, 900.0, 3.0);
    f.s.note_layer_costs(f.outer, [(f.outer, f.solid, f64::NAN), (f.outer, f.solid, -4.0), (f.outer, LayerId(9999), 900.0)]);
    f.s.note_layer_costs(ItemId(9999), [(f.outer, f.solid, 900.0)]);
    let found = f.s.freeze_suggestions();
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!((found[0].layer, found[0].precomp), (f.solid, None));
    assert_eq!(found[0].freeze_params()["mode"], "selection");
}

#[test]
fn dismissing_hides_a_suggestion_and_freezing_it_clears_it_with_undo() {
    let mut f = fixture();
    play(&mut f, 12, 1800.0, 2.0, 2.0);
    f.s.execute("freeze.suggest", json!({"dismiss":{"comp":f.outer.0, "layer":f.stack.0}})).unwrap();
    assert!(f.s.freeze_suggestions().is_empty());

    let mut f = fixture();
    play(&mut f, 12, 1800.0, 2.0, 2.0);
    let mut params = f.s.freeze_suggestions()[0].freeze_params();
    params["wait"] = json!(true);
    params["quickDraft"] = json!(false);
    f.s.execute("freeze.freeze", params).unwrap();
    let section = &f.s.project.freeze.sections[0];
    assert!(section.auto && section.cache.is_some(), "the suggestion's freeze keeps itself fresh (Auto)");
    assert!(f.s.freeze_suggestions().is_empty(), "a frozen layer is not suggested again");
    assert!(f.s.undo(), "freezing is an undo step");
    assert!(f.s.project.freeze.sections.is_empty() || f.s.project.freeze.sections[0].cache.is_none());
    std::fs::remove_dir_all(crate::freeze_storage::root(&f.s.project.freeze.namespace).unwrap()).ok();
}

#[test]
fn heavy_layers_freeze_by_themselves_only_with_the_preference_and_when_idle() {
    let mut f = fixture();
    play(&mut f, 12, 1800.0, 2.0, 2.0);
    idle_for(&mut f.s, 30);
    f.s.poll_freeze();
    f.s.wait_jobs();
    assert!(f.s.project.freeze.sections.is_empty(), "preference off: nothing freezes without a click");

    f.s.prefs.previews.freeze_auto_heavy = true;
    idle_for(&mut f.s, 2);
    f.s.poll_freeze();
    assert!(f.s.project.freeze.sections.is_empty(), "not idle yet");

    idle_for(&mut f.s, 30);
    f.s.poll_freeze();
    f.s.wait_jobs();
    assert_eq!(f.s.project.freeze.sections.len(), 1);
    assert!(f.s.project.freeze.sections[0].auto);
    assert!(f.s.project.freeze.sections[0].cache.is_some());
    std::fs::remove_dir_all(crate::freeze_storage::root(&f.s.project.freeze.namespace).unwrap()).ok();
}

fn idle_for(s: &mut Session, seconds: u64) {
    let ago = web_time::Instant::now() - std::time::Duration::from_secs(seconds);
    s.freeze_runtime.changed = ago;
    s.freeze_runtime.activity = ago;
}
