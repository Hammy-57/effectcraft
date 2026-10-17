//! Settings ▸ General ▸ Synchronize Time of All Related Items: moving one comp's time moves the
//! comps nested in it and the comps nesting it, through each precomp layer's timing.

use effectcraft_project::ItemId;
use effectcraft_time::Tick;
use serde_json::json;

use crate::Session;

fn comp(s: &mut Session, name: &str) -> ItemId {
    ItemId(s.execute("comp.new", json!({"name": name, "width": 32, "height": 18, "frameRate": 25, "duration": 30})).unwrap()["comp"].as_u64().unwrap())
}

/// Adds `item` to the active comp starting at `start` seconds.
fn nest(s: &mut Session, item: ItemId, start: f64) {
    let l = s.execute("layer.addItem", json!({"item": item.0})).unwrap()["layer"].as_u64().unwrap();
    s.execute("layer.timing", json!({"layers": [l], "start": start})).unwrap();
}

fn secs(t: f64) -> Tick {
    Tick::from_seconds_f64(t)
}

/// A nests B twice (at 2 s, then 5 s), B nests D (at 1 s), C nests B twice (at 3 s, then 6 s), E
/// nests A (at 4 s), F is unrelated. A comp's topmost precomp layer (the one added last) counts.
#[test]
fn moving_a_comps_time_moves_the_comps_nested_in_it_and_nesting_it() {
    let mut s = Session::default();
    let d = comp(&mut s, "D");
    let b = comp(&mut s, "B");
    nest(&mut s, d, 1.0);
    let a = comp(&mut s, "A");
    nest(&mut s, b, 2.0);
    nest(&mut s, b, 5.0);
    let c = comp(&mut s, "C");
    nest(&mut s, b, 3.0);
    nest(&mut s, b, 6.0);
    let e = comp(&mut s, "E");
    nest(&mut s, a, 4.0);
    let f = comp(&mut s, "F");
    s.execute("time.set", json!({"time": 7.0})).unwrap();
    assert!(s.prefs.general.sync_time_related_items);
    s.execute("comp.open", json!({"comp": a.0})).unwrap();
    s.execute("time.set", json!({"time": 10.0})).unwrap();
    let times = [a, b, c, d, e, f].map(|id| s.time_of(id));
    assert_eq!(times, [secs(10.0), secs(5.0), secs(11.0), secs(4.0), secs(14.0), secs(7.0)]);
    // From the leaf up: D at 3 s is B at 4 s, which is A at 9 s and C at 10 s; A is E at 13 s.
    s.execute("comp.open", json!({"comp": d.0})).unwrap();
    s.execute("time.set", json!({"time": 3.0})).unwrap();
    let times = [a, b, c, d, e, f].map(|id| s.time_of(id));
    assert_eq!(times, [secs(9.0), secs(4.0), secs(10.0), secs(3.0), secs(13.0), secs(7.0)]);
}
