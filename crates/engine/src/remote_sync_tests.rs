//! Tests of the replica sync ([`crate::remote_sync`]): random edits end with matching hashes,
//! damaged patches resync, drags send one small patch per frame. The benchmark on a real
//! project is `sync_bench_on_a_real_project` (ignored; `EFFECTCRAFT_SYNC_BENCH=path.ecproj`).

use std::sync::Arc;

use crate::color::Label;
use effectcraft_keyframe::{Keyframe, Value as KValue};
use effectcraft_project::{Comp, ItemId, ItemKind, Layer, Project};
use effectcraft_time::Tick;
use serde_json::json;

use crate::perf::{LargeSpec, large_project};
use crate::remote::{FrameMsg, FrameReply, FrameServer, Mirror, diff, patch};
use crate::remote_sync::*;

/// xorshift64*: deterministic random numbers for the property test.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
    fn below(&mut self, n: usize) -> usize {
        if n == 0 { 0 } else { (self.next() % n as u64) as usize }
    }
    fn chance(&mut self, one_in: u64) -> bool {
        self.next().is_multiple_of(one_in)
    }
}

fn server() -> FrameServer {
    let s = crate::Session::default();
    FrameServer::new(s.footage.clone(), s.expr.clone())
}

/// Through the byte encoding, as on the wire.
fn wire(m: &FrameMsg) -> FrameMsg {
    decode_msg(&encode_msg(m)).unwrap()
}

fn comps_with_layers(p: &Project) -> Vec<ItemId> {
    p.comps().filter(|(_, c)| !c.layers.is_empty()).map(|(id, _)| *id).collect()
}

/// One random edit, copy-on-write as the session makes them (the project is cloned, then only
/// what changes is made unique).
fn random_edit(rng: &mut Rng, p: &mut Project, history: &[Arc<Project>]) {
    let comps: Vec<ItemId> = p.comps().map(|(id, _)| *id).collect();
    let full = comps_with_layers(p);
    let pick = |rng: &mut Rng, v: &[ItemId]| v.get(rng.below(v.len())).copied();
    match rng.below(16) {
        // A keyframe or value drag.
        0..=4 => {
            let Some(cid) = pick(rng, &full) else { return };
            let n = p.comp(cid).map_or(0, |c| c.layers.len());
            let li = rng.below(n);
            let k = rng.next();
            let Some(l) = p.comp_mut(cid).and_then(|c| c.layers.get_mut(li)) else { return };
            let mut props = vec![];
            l.props.walk_mut(&mut |pr| props.push(pr as *mut _ as usize));
            let target = props.get((k as usize) % props.len().max(1)).copied();
            l.props.walk_mut(&mut |pr| {
                if Some(pr as *mut _ as usize) != target {
                    return;
                }
                let dt = Tick::from_seconds_f64(1.0 / 30.0);
                match pr.keys.first_mut() {
                    Some(key) if k.is_multiple_of(2) => key.time += dt,
                    _ => {
                        if let KValue::Scalar(v) = &mut pr.value {
                            *v += 1.5;
                        } else {
                            pr.value = KValue::Scalar((k % 100) as f64);
                        }
                    }
                }
            });
        }
        // Add or remove a keyframe.
        5 => {
            let Some(cid) = pick(rng, &full) else { return };
            let n = p.comp(cid).map_or(0, |c| c.layers.len());
            let li = rng.below(n);
            let add = rng.chance(2);
            let Some(l) = p.comp_mut(cid).and_then(|c| c.layers.get_mut(li)) else { return };
            let mut done = false;
            l.props.walk_mut(&mut |pr| {
                if done {
                    return;
                }
                done = true;
                if add || pr.keys.is_empty() {
                    let t = pr.keys.last().map_or(Tick::ZERO, |k| k.time) + Tick::from_seconds_f64(0.5);
                    pr.keys.push(Keyframe::new(t, pr.value.clone()));
                } else {
                    pr.keys.remove(0);
                }
            });
        }
        // Duplicate a layer (a new id), possibly into another comp.
        6 => {
            let (Some(from), Some(to)) = (pick(rng, &full), pick(rng, &comps)) else { return };
            let Some(c) = p.comp(from) else { return };
            let Some(mut l) = c.layers.get(rng.below(c.layers.len())).cloned() else { return };
            l.id = effectcraft_project::LayerId(p.alloc());
            l.name = format!("Copy {}", l.id.0);
            let at = rng.below(p.comp(to).map_or(0, |c| c.layers.len() + 1));
            if let Some(c) = p.comp_mut(to) {
                c.layers.insert(at.min(c.layers.len()), l);
            }
        }
        // Delete a layer.
        7 => {
            let Some(cid) = pick(rng, &full) else { return };
            let i = rng.below(p.comp(cid).map_or(0, |c| c.layers.len()));
            if let Some(c) = p.comp_mut(cid)
                && i < c.layers.len()
            {
                c.layers.remove(i);
            }
        }
        // Reorder.
        8 => {
            let Some(cid) = pick(rng, &full) else { return };
            let n = p.comp(cid).map_or(0, |c| c.layers.len());
            let (a, b) = (rng.below(n), rng.below(n));
            if let Some(c) = p.comp_mut(cid) {
                c.layers.swap(a, b);
            }
        }
        // The comp's own settings, or its item's name.
        9 => {
            let Some(cid) = pick(rng, &comps) else { return };
            if rng.chance(2) {
                if let Some(c) = p.comp_mut(cid) {
                    c.background = [rng.below(100) as f32 / 100.0, 0.5, 0.25];
                    c.duration += Tick::from_seconds_f64(1.0);
                }
            } else if let Some(it) = p.item_mut(cid) {
                it.name = format!("Comp {}", rng.below(1000));
                it.label = Label::Blue;
            }
        }
        // A new comp with a few layers from another.
        10 => {
            let mut c = Comp::new(640, 360, effectcraft_time::FrameRate::FPS_30, Tick::from_seconds_f64(5.0));
            if let Some(src) = pick(rng, &full).and_then(|id| p.comp(id)) {
                for l in src.layers.iter().take(3).cloned().collect::<Vec<Layer>>() {
                    c.layers.push(l);
                }
            }
            p.add_item("New comp", Label::None, None, ItemKind::Comp(Arc::new(c)));
        }
        // Delete an item (keep at least a few comps).
        11 => {
            let ids: Vec<ItemId> = p.items.keys().copied().collect();
            if comps.len() > 3
                && let Some(id) = pick(rng, &ids)
            {
                p.items.remove(&id);
            }
        }
        // Another item kind changes (footage or folder name).
        12 => {
            let ids: Vec<ItemId> = p.items.iter().filter(|(_, i)| i.as_comp().is_none()).map(|(id, _)| *id).collect();
            if let Some(it) = pick(rng, &ids).and_then(|id| p.item_mut(id)) {
                it.comment = format!("note {}", rng.below(1000));
            }
        }
        // Project settings and the id counter.
        13 => {
            p.settings.gpu_acceleration = !p.settings.gpu_acceleration;
            p.alloc();
        }
        // Undo: back to an older snapshot.
        14 => {
            if let Some(old) = history.get(rng.below(history.len())) {
                *p = (**old).clone();
            }
        }
        // The same layer twice in a stack (shared), or a layer shared by two comps.
        _ => {
            let (Some(from), Some(to)) = (pick(rng, &full), pick(rng, &comps)) else { return };
            let Some(arc) = p.comp(from).and_then(|c| c.layers.arcs().first().cloned()) else { return };
            if let Some(c) = p.comp_mut(to) {
                c.layers.arcs_mut().push(arc);
            }
        }
    }
}

/// Sync `p` to the server; on a resync, again with the whole project. Returns the bytes sent
/// and whether a resync happened.
fn sync(m: &mut Mirror, w: &mut FrameServer, rev: u64, p: &Arc<Project>) -> (usize, bool) {
    let Some(msg) = m.sync(rev, p) else { return (0, false) };
    let bytes = encode_msg(&msg).len();
    let replies = w.handle(wire(&msg));
    if replies.iter().any(|(r, _)| matches!(r, FrameReply::Resync { .. })) {
        m.reset();
        let full = m.sync(rev, p).unwrap();
        assert!(matches!(full, FrameMsg::Project { .. }));
        assert!(w.handle(wire(&full)).is_empty());
        return (bytes, true);
    }
    assert!(replies.is_empty(), "{:?}", replies.first().map(|r| &r.0));
    (bytes, false)
}

/// 1,000 random edits (keyframe and value drags, keyframes added and removed, layers added,
/// deleted, reordered and shared, comps and items added, renamed and deleted, settings, undo to
/// older snapshots), synced in random batches through the wire encoding: the replica's hash
/// always matches the project's, and it never needed the whole project again.
#[test]
fn random_edits_keep_the_replica_identical() {
    let mut rng = Rng(0x00c0_ffee_1234_5678);
    let mut p = Arc::new(large_project(&LargeSpec::small()));
    let mut history = vec![p.clone()];
    let (mut m, mut w) = (Mirror::default(), server());
    let mut rev = 1;
    let mut mac = SyncCache::default();
    sync(&mut m, &mut w, rev, &p);
    let (mut patches, mut total) = (0, 0);
    for i in 0..1000 {
        let mut next = (*p).clone();
        random_edit(&mut rng, &mut next, &history);
        p = Arc::new(next);
        rev += 1;
        history.push(p.clone());
        if history.len() > 20 {
            history.remove(0);
        }
        if rng.chance(3) || i == 999 {
            let (bytes, resynced) = sync(&mut m, &mut w, rev, &p);
            assert!(!resynced, "edit {i}: the replica asked for the whole project");
            patches += 1;
            total += bytes;
            if patches % 10 == 0 || i == 999 {
                assert_eq!(w.project_hash(), Some(project_hash(&p, &mut mac)), "edit {i}");
            }
        }
    }
    assert_eq!(**w.project().unwrap(), *p);
    assert_eq!(w.revision(), Some(rev));
    let whole = serde_json::to_string(&*p).unwrap().len();
    assert!(total / patches < whole / 4, "{patches} patches, {total} bytes, project {whole}");
    // The caches only hold what the snapshots still share (here: no undo history any more).
    drop(history);
    mac.trim();
    assert!(mac.len() <= p.comps().map(|(_, c)| c.layers.len()).sum::<usize>() * 2 + 64);
}

/// A damaged patch (a wrong hash, a missing base) is answered with a resync, and after the
/// whole project the replica matches again.
#[test]
fn a_hash_mismatch_resyncs() {
    let mut p = Arc::new(large_project(&LargeSpec::small()));
    let (mut m, mut w) = (Mirror::default(), server());
    sync(&mut m, &mut w, 1, &p);
    let cid = comps_with_layers(&p)[0];
    let mut next = (*p).clone();
    next.comp_mut(cid).unwrap().layers.get_mut(0).unwrap().name = "Edited".into();
    p = Arc::new(next);
    let Some(FrameMsg::Patch(mut pt)) = m.sync(2, &p) else { panic!("expected a patch") };
    let PatchOp::Comp { hash, .. } = &mut pt.ops[0] else { panic!("{:?}", pt.ops[0]) };
    *hash ^= 1;
    let out = w.handle(wire(&FrameMsg::Patch(pt)));
    assert!(matches!(&out[0].0, FrameReply::Resync { error } if error.contains("hash")), "{:?}", out[0].0);
    assert!(w.project().is_none() && w.revision().is_none());
    m.reset();
    assert!(w.handle(wire(&m.sync(2, &p).unwrap())).is_empty());
    assert_eq!(w.project_hash(), Some(project_hash(&p, &mut SyncCache::default())));
    // A layer run beyond the old stack, a layer edit of something else: resync, never a panic.
    for ops in [
        vec![PatchOp::Comp { id: cid, shell: None, layers: Some(vec![LayerOp::Keep { from: 10_000, count: usize::MAX }]), hash: 0 }],
        vec![PatchOp::Comp { id: cid, shell: None, layers: Some(vec![LayerOp::Edit { from: 0, ops: diff(&json!(1), &json!([1, 2])) }]), hash: 0 }],
        vec![PatchOp::Comp { id: ItemId(u64::MAX), shell: None, layers: None, hash: 0 }],
        vec![PatchOp::Remove { id: ItemId(u64::MAX) }],
        vec![PatchOp::Item { item: json!({"nope": 1}), hash: 0 }],
        vec![PatchOp::Head { head: json!(null), hash: 0 }],
    ] {
        let out = w.handle(FrameMsg::Patch(Patch { from_rev: 2, to_rev: 3, ops }));
        assert!(matches!(out[0].0, FrameReply::Resync { .. }), "{:?}", out[0].0);
        assert!(w.handle(wire(&FrameMsg::Project { revision: 2, project: serde_json::to_string(&*p).unwrap() })).is_empty());
    }
}

/// Splices: a keyframe added in the middle of a long list sends that keyframe, not the list.
#[test]
fn array_splices_round_trip() {
    let a = json!({"k": [1, 2, 3, 4, 5, 6, 7, 8], "x": [1, 2]});
    for b in [
        json!({"k": [1, 2, 3, 99, 4, 5, 6, 7, 8], "x": [1, 2]}),
        json!({"k": [1, 2, 3, 5, 6, 7, 8], "x": [2]}),
        json!({"k": [0, 1, 2, 3, 4, 5, 6, 7, 8, 9], "x": []}),
        json!({"k": [], "x": [1, 2, 3]}),
        json!({"k": [8, 7], "x": [1, 1, 2, 2]}),
    ] {
        let ops = diff(&a, &b);
        let mut x = a.clone();
        patch(&mut x, &serde_json::from_str::<Vec<_>>(&serde_json::to_string(&ops).unwrap()).unwrap()).unwrap();
        assert_eq!(x, b, "{ops:?}");
    }
    let ops = diff(&a, &json!({"k": [1, 2, 3, 99, 4, 5, 6, 7, 8], "x": [1, 2]}));
    assert_eq!(serde_json::to_string(&ops).unwrap(), r#"[{"path":["k"],"value":[99],"splice":[3,0]}]"#);
    // Damaged splices fail cleanly.
    let bad = |path: serde_json::Value, splice: [usize; 2]| {
        serde_json::from_value::<crate::remote::Op>(json!({"path": path, "value": [], "splice": splice})).unwrap()
    };
    for op in [bad(json!(["k"]), [7, 5]), bad(json!(["k"]), [usize::MAX, 2]), bad(json!(["nope"]), [0, 0]), bad(json!([]), [0, 0])] {
        assert!(patch(&mut a.clone(), &[op]).is_err());
    }
}

/// The wire encoding: a whole project goes zstd-compressed and comes back the same; long
/// messages are compressed; damaged bytes are an error, not a panic.
#[test]
fn messages_encode_compressed() {
    let p = large_project(&LargeSpec::small());
    let json = serde_json::to_string(&p).unwrap();
    let msg = FrameMsg::Project { revision: 7, project: json.clone() };
    let bytes = encode_msg(&msg);
    assert_eq!(bytes[0], b'p');
    assert!(bytes.len() * 5 < json.len(), "{} of {}", bytes.len(), json.len());
    let FrameMsg::Project { revision: 7, project } = decode_msg(&bytes).unwrap() else { panic!() };
    assert_eq!(project, json);
    assert_eq!(encode_msg(&FrameMsg::Purge)[0], b'j');
    let long = FrameMsg::Segs { segs: vec![crate::offload::SegData { key: 1, data: "A".repeat(100_000) }] };
    let b = encode_msg(&long);
    assert!(b[0] == b'z' && b.len() < 10_000);
    assert!(matches!(decode_msg(&b).unwrap(), FrameMsg::Segs { segs } if segs[0].data.len() == 100_000));
    for bad in [&b""[..], b"x{}", b"j{", b"z\x28\xb5\x2f\xfd garbage", b"p\x01\x02", b"p12345678nozstd"] {
        assert!(decode_msg(bad).is_err(), "{bad:?}");
    }
    let mut cut = bytes.clone();
    cut.truncate(bytes.len() / 2);
    assert!(decode_msg(&cut).is_err());
}

#[test]
fn stable_hash_ignores_how_bytes_are_split() {
    let data: Vec<u8> = (0..1000u32).map(|i| (i * 7 % 251) as u8).collect();
    let mut one = StableHasher::default();
    one.write(&data);
    for split in [1, 3, 7, 8, 9, 500] {
        let mut h = StableHasher::default();
        for c in data.chunks(split) {
            h.write(c);
        }
        assert_eq!(h.finish(), one.finish(), "chunks of {split}");
    }
    let mut other = StableHasher::default();
    other.write(&data[..999]);
    assert_ne!(other.finish(), one.finish());
    // Fixed across platforms and builds (the Mac and the server must agree).
    let mut h = StableHasher::default();
    h.write(b"effectcraft");
    assert_eq!(h.finish(), 0x236d_fd19_45a0_d7b2, "{:#x}", h.finish());
}

/// A drag: 120 revisions in 0.5 s become at most one patch per 1/60 s, the last one carrying
/// the latest revision, each a few hundred bytes.
#[test]
fn a_drag_coalesces_to_one_small_patch_per_frame() {
    let mut p = Arc::new(large_project(&LargeSpec::small()));
    let (mut m, mut w) = (Mirror::default(), server());
    sync(&mut m, &mut w, 1, &p);
    let cid = comps_with_layers(&p)[0];
    let mut co = Coalescer::default();
    let t0 = web_time::Instant::now();
    let mut sent = vec![];
    for i in 0..120u64 {
        let mut next = (*p).clone();
        if let Some(l) = next.comp_mut(cid).and_then(|c| c.layers.get_mut(0)) {
            l.props.walk_mut(&mut |pr| {
                if pr.match_id == "position" {
                    pr.value = KValue::Vec2([i as f64, 2.0 * i as f64]);
                }
            });
        }
        p = Arc::new(next);
        co.offer(i + 2, &p);
        // 4 ms between mouse moves.
        let now = t0 + web_time::Duration::from_millis(4 * i);
        if let Some((rev, proj)) = co.take(now) {
            let msg = m.sync(rev, &proj).unwrap();
            sent.push((rev, encode_msg(&msg).len()));
            assert!(w.handle(wire(&msg)).is_empty());
        }
    }
    if let Some((rev, proj)) = co.flush(t0 + web_time::Duration::from_millis(480)) {
        sent.push((rev, encode_msg(&m.sync(rev, &proj).unwrap()).len()));
        w.handle(wire(&FrameMsg::Purge));
    }
    assert!(sent.len() <= 480 / 16 + 2, "{} patches", sent.len());
    assert!(sent.len() >= 20);
    assert!(sent.iter().all(|(_, b)| *b < 2_000), "{sent:?}");
    assert!(co.take(t0 + web_time::Duration::from_secs(9)).is_none() && co.due().is_none());
}

/// Bytes and milliseconds per patch, before (the whole project as JSON, diffed and re-parsed)
/// and after, for a keyframe drag on the largest layer of a real project.
/// `EFFECTCRAFT_SYNC_BENCH=project.ecproj cargo test -p effectcraft-engine --release --lib
/// remote_sync_tests::sync_bench -- --ignored --nocapture`
#[test]
#[ignore]
fn sync_bench_on_a_real_project() {
    use web_time::Instant;
    let Ok(path) = std::env::var("EFFECTCRAFT_SYNC_BENCH") else { return };
    let text = std::fs::read_to_string(&path).unwrap();
    let base = Arc::new(Project::from_json(&text).unwrap());
    let json_len = serde_json::to_string(&*base).unwrap().len();
    // The largest layer with keyframes.
    let mut best: Option<(usize, ItemId, usize)> = None;
    for (id, c) in base.comps() {
        for (i, l) in c.layers.iter().enumerate() {
            let mut keyed = false;
            l.props.walk("", &mut |_, pr| keyed |= !pr.keys.is_empty());
            let n = serde_json::to_string(l).unwrap().len();
            if keyed && best.is_none_or(|b| n > b.0) {
                best = Some((n, *id, i));
            }
        }
    }
    let (layer_bytes, cid, li) = best.unwrap();
    println!("project {path}: {:.1} MB JSON, {} items; dragging a key of a {:.0} KB layer", json_len as f64 / 1e6, base.items.len(), layer_bytes as f64 / 1e3);
    let drag = |p: &Arc<Project>, step: u64| {
        let mut next = (**p).clone();
        if let Some(l) = next.comp_mut(cid).and_then(|c| c.layers.get_mut(li)) {
            let mut done = false;
            l.props.walk_mut(&mut |pr| {
                if !done && let Some(k) = pr.keys.last_mut() {
                    k.time += Tick::from_seconds_f64(step as f64 / 30.0);
                    done = true;
                }
            });
        }
        Arc::new(next)
    };
    const STEPS: u64 = 30;
    // Before: the old sync (the whole project to a JSON value, a structural diff, the replica
    // patched and re-parsed).
    {
        let t = Instant::now();
        let mut old_doc = serde_json::to_value(&*base).unwrap();
        let mut replica = old_doc.clone();
        let mut p = base.clone();
        let (mut send_ms, mut recv_ms, mut bytes) = (0.0, 0.0, 0);
        for i in 0..STEPS {
            p = drag(&p, i);
            let t0 = Instant::now();
            let v = serde_json::to_value(&*p).unwrap();
            let ops = diff(&old_doc, &v);
            let msg = serde_json::to_string(&ops).unwrap();
            old_doc = v;
            let t1 = Instant::now();
            patch(&mut replica, &serde_json::from_str::<Vec<_>>(&msg).unwrap()).unwrap();
            let _ = Project::from_json(&replica.to_string()).unwrap();
            let t2 = Instant::now();
            send_ms += (t1 - t0).as_secs_f64() * 1e3;
            recv_ms += (t2 - t1).as_secs_f64() * 1e3;
            bytes += msg.len();
        }
        let n = STEPS as f64;
        println!(
            "before: {:.0} B/patch, send {:.1} ms + receive {:.1} ms per patch ({:.1} s total)",
            bytes as f64 / n,
            send_ms / n,
            recv_ms / n,
            t.elapsed().as_secs_f64()
        );
    }
    // After.
    let (mut m, mut w) = (Mirror::default(), server());
    let t0 = Instant::now();
    let first = m.sync(1, &base).unwrap();
    let t1 = Instant::now();
    let enc = encode_msg(&first);
    let t2 = Instant::now();
    let dec = decode_msg(&enc).unwrap();
    let t3 = Instant::now();
    assert!(w.handle(dec).is_empty());
    let t4 = Instant::now();
    let ms = |a: Instant, b: Instant| (b - a).as_secs_f64() * 1e3;
    println!(
        "whole project (first sync / resync): {:.1} MB JSON → {:.2} MB zstd; serialize {:.0} ms, compress {:.0} ms, decompress {:.0} ms, load {:.0} ms",
        json_len as f64 / 1e6,
        enc.len() as f64 / 1e6,
        ms(t0, t1),
        ms(t1, t2),
        ms(t2, t3),
        ms(t3, t4)
    );
    let mut p = base.clone();
    let mut rows = vec![];
    for i in 0..STEPS {
        p = drag(&p, i);
        let a = Instant::now();
        let msg = m.sync(i + 2, &p).unwrap();
        let bytes = encode_msg(&msg);
        let b = Instant::now();
        let out = w.handle(decode_msg(&bytes).unwrap());
        let c = Instant::now();
        assert!(out.is_empty(), "{:?}", out[0].0);
        rows.push((bytes.len(), ms(a, b), ms(b, c)));
    }
    let max = rows.iter().map(|r| r.0).max().unwrap();
    let n = rows.len() as f64;
    let steady = &rows[1..];
    let sn = steady.len() as f64;
    println!(
        "after: {:.0} B/patch (max {max}), send {:.2} ms + receive {:.2} ms per patch; first patch send {:.1} ms + receive {:.1} ms (hashes the comp once)",
        rows.iter().map(|r| r.0).sum::<usize>() as f64 / n,
        steady.iter().map(|r| r.1).sum::<f64>() / sn,
        steady.iter().map(|r| r.2).sum::<f64>() / sn,
        rows[0].1,
        rows[0].2
    );
    assert!(max < 20_000, "{max} bytes");
    assert_eq!(w.project_hash(), Some(project_hash(&p, &mut SyncCache::default())));
}
