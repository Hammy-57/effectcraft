//! The layer cache keeps precomps and footage, memoises key structure and renders concurrent
//! misses once.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use effectcraft_color::Label;
use effectcraft_keyframe::{Keyframe, Value};
use effectcraft_project::build::{self, Ids};
use effectcraft_project::{BitDepth, Comp, Expression, Footage, FootageKind, ItemId, ItemKind, Layer, LayerSource, Project, Property, Solid};
use effectcraft_time::{FrameRate, Tick};

use crate::{EvalCtx, ExprHost, FootageSource, Image, LayerCache, NoFootage, RenderOpts, Renderer};

fn s(x: f64) -> Tick {
    Tick::from_seconds_f64(x)
}

fn comp(p: &mut Project, name: &str) -> (ItemId, Comp) {
    let c = Comp::new(100, 80, FrameRate::FPS_30, s(4.0));
    (p.add_item(name, Label::Sandstone, None, ItemKind::Comp(c.clone().into())), c)
}

fn project() -> Project {
    let mut p = Project::default();
    p.settings.bit_depth = BitDepth::Bpc32;
    p
}

fn solid(p: &mut Project, c: &Comp, color: [f32; 3]) -> Layer {
    let sid = p.add_item("S", Label::Red, None, ItemKind::Solid(Solid { color, width: 40, height: 30, pixel_aspect: 1.0 }));
    build::layer(p, c, "S", LayerSource::Solid { item: sid }, (40, 30), None)
}

fn blur(p: &mut Project, l: &mut Layer, amount: f64) {
    let spec = effectcraft_effects::find("ec.blur.gaussian").unwrap();
    let mut next = p.next_id;
    let mut g = effectcraft_effects::instantiate(spec, &mut Ids(&mut next), spec.name, [40.0, 30.0]);
    p.next_id = next;
    g.prop_mut("blurriness").unwrap().value = Value::Scalar(amount);
    l.props.sub_mut("effects").unwrap().children.push(g.into());
}

fn moving(l: &mut Layer) {
    l.props.prop_mut("transform/position").unwrap().keys =
        vec![Keyframe::new(Tick::ZERO, Value::Vec3([20.0, 20.0, 0.0])), Keyframe::new(s(2.0), Value::Vec3([80.0, 60.0, 0.0]))];
}

/// An outer comp moving a precomp of a blurred solid. Returns (project, outer, inner).
fn precomp_scene() -> (Project, ItemId, ItemId) {
    let mut p = project();
    let (iid, inner) = comp(&mut p, "Inner");
    let mut l = solid(&mut p, &inner, [1.0, 0.5, 0.0]);
    blur(&mut p, &mut l, 6.0);
    p.comp_mut(iid).unwrap().layers.push(l);
    let (oid, outer) = comp(&mut p, "Outer");
    let mut pre = build::layer(&mut p, &outer, "Inner", LayerSource::Comp { item: iid }, (100, 80), None);
    moving(&mut pre);
    p.comp_mut(oid).unwrap().layers.push(pre);
    (p, oid, iid)
}

fn frame(p: &Project, cid: ItemId, t: f64, footage: &dyn FootageSource, expr: Option<&dyn ExprHost>, cache: Option<&LayerCache>) -> Image {
    let mut r = Renderer::new(p, footage, RenderOpts::default());
    r.cache = cache;
    r.expr = expr;
    r.comp_frame(cid, s(t))
}

fn assert_same(a: &Image, b: &Image, what: &str) {
    assert_eq!((a.width, a.height), (b.width, b.height), "{what}");
    for (i, (p, q)) in a.data.iter().zip(&b.data).enumerate() {
        assert!((0..4).all(|c| (p[c] - q[c]).abs() < 1e-6), "{what}: pixel {i}: {p:?} vs {q:?}");
    }
}

#[test]
fn a_static_precomp_renders_its_nested_comp_once() {
    let (p, oid, _) = precomp_scene();
    let cache = LayerCache::default();
    let first = frame(&p, oid, 0.0, &NoFootage, None, Some(&cache));
    assert_same(&first, &frame(&p, oid, 0.0, &NoFootage, None, None), "t=0");
    let rendered = cache.inserts();
    assert!(rendered >= 2, "the nested solid and the precomp were kept: {rendered}");
    for t in [0.5, 1.0, 1.5] {
        let before = cache.stats();
        let img = frame(&p, oid, t, &NoFootage, None, Some(&cache));
        assert_same(&img, &frame(&p, oid, t, &NoFootage, None, None), &format!("t={t}"));
        let after = cache.stats();
        // One lookup: the precomp's buffer (the nested comp isn't drawn again).
        assert_eq!((after.hits - before.hits, after.misses - before.misses), (1, 0), "t={t}");
    }
    assert_eq!(cache.inserts(), rendered, "nothing rendered again");
}

#[test]
fn an_animated_nested_layer_still_updates() {
    let (mut p, oid, iid) = precomp_scene();
    moving(&mut p.comp_mut(iid).unwrap().layers[0]);
    let cache = LayerCache::default();
    let mut last: Option<Image> = None;
    for t in [0.0, 0.5, 1.0, 0.5] {
        let img = frame(&p, oid, t, &NoFootage, None, Some(&cache));
        assert_same(&img, &frame(&p, oid, t, &NoFootage, None, None), &format!("t={t}"));
        if let Some(prev) = &last {
            assert_ne!(prev.data, img.data, "t={t}: the nested layer moved");
        }
        last = Some(img);
    }
}

/// An expression whose result comes from outside the nested comp (as one reading another
/// comp's layer would): the value it returns is part of the key.
struct Outside(AtomicU64);
impl ExprHost for Outside {
    fn eval(&self, _: &EvalCtx, _: &Layer, _: &Property, _: &Value) -> Result<Value, String> {
        Ok(Value::Scalar(f64::from_bits(self.0.load(Ordering::SeqCst))))
    }
}

#[test]
fn a_nested_expression_reading_outside_is_followed() {
    let (mut p, oid, iid) = precomp_scene();
    p.comp_mut(iid).unwrap().layers[0].props.prop_mut("effects/#1/blurriness").unwrap().expr =
        Some(Expression { text: "comp(\"Outer\").layer(1).transform.opacity".into(), enabled: true });
    let host = Outside(AtomicU64::new(2.0f64.to_bits()));
    let cache = LayerCache::default();
    let a = frame(&p, oid, 0.0, &NoFootage, Some(&host), Some(&cache));
    host.0.store(12.0f64.to_bits(), Ordering::SeqCst);
    let b = frame(&p, oid, 0.0, &NoFootage, Some(&host), Some(&cache));
    assert_same(&b, &frame(&p, oid, 0.0, &NoFootage, Some(&host), None), "after the outside value changed");
    assert_ne!(a.data, b.data, "the blur follows the expression");
}

#[test]
fn editing_a_nested_layer_in_place_rerenders_the_precomp() {
    let (mut p, oid, iid) = precomp_scene();
    let cache = LayerCache::default();
    frame(&p, oid, 0.5, &NoFootage, None, Some(&cache));
    // The same project edited in place (the layer's and the comp's allocations are not shared
    // with another snapshot): the memo must not take them for the old ones.
    p.comp_mut(iid).unwrap().layers[0].props.prop_mut("effects/#1/blurriness").unwrap().value = Value::Scalar(20.0);
    let img = frame(&p, oid, 0.5, &NoFootage, None, Some(&cache));
    assert_same(&img, &frame(&p, oid, 0.5, &NoFootage, None, None), "after the edit");
    let k = |p: &Project| {
        let c = p.comp(iid).unwrap();
        let ctx = EvalCtx::new(p, iid, c, s(0.5));
        crate::cache::layer_key(&ctx, &c.layers[0], 1.0, false, false)
    };
    let before = k(&p);
    assert_eq!(before, k(&p), "an unchanged layer keeps its key");
    p.comp_mut(iid).unwrap().layers[0].props.prop_mut("effects/#1/blurriness").unwrap().value = Value::Scalar(3.0);
    assert_ne!(before, k(&p), "the edit changes the key");
}

/// A still that counts its decodes (and takes a while, so concurrent renders overlap).
struct CountingStill {
    reads: AtomicUsize,
    delay_ms: u64,
}
impl FootageSource for CountingStill {
    fn frame(&self, _: ItemId, f: &Footage, _: Tick) -> Option<Arc<Image>> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        std::thread::sleep(std::time::Duration::from_millis(self.delay_ms));
        Some(Arc::new(Image::filled(f.width, f.height, [0.2, 0.4, 0.8, 1.0])))
    }
}

/// A comp with a blurred still that moves. Returns (project, comp).
fn still_scene() -> (Project, ItemId) {
    let mut p = project();
    let (cid, c) = comp(&mut p, "Comp");
    let f = Footage { path: "still.png".into(), kind: FootageKind::Still, width: 40, height: 30, pixel_aspect: 1.0, has_video: true, loop_count: 1, ..Default::default() };
    let fid = p.add_item("Still", Label::Aqua, None, ItemKind::Footage(f));
    let mut l = build::layer(&mut p, &c, "Still", LayerSource::Footage { item: fid }, (40, 30), None);
    blur(&mut p, &mut l, 8.0);
    moving(&mut l);
    p.comp_mut(cid).unwrap().layers.push(l);
    (p, cid)
}

#[test]
fn a_blurred_still_is_blurred_once() {
    let (p, cid) = still_scene();
    let src = CountingStill { reads: AtomicUsize::new(0), delay_ms: 0 };
    let cache = LayerCache::default();
    for t in [0.0, 0.4, 0.8, 1.2] {
        let img = frame(&p, cid, t, &src, None, Some(&cache));
        let fresh = CountingStill { reads: AtomicUsize::new(0), delay_ms: 0 };
        assert_same(&img, &frame(&p, cid, t, &fresh, None, None), &format!("t={t}"));
    }
    assert_eq!(src.reads.load(Ordering::SeqCst), 1, "decoded and blurred on the first frame only");
    assert_eq!(cache.inserts(), 1);
}

#[test]
fn concurrent_misses_render_once() {
    let (p, cid) = still_scene();
    let src = CountingStill { reads: AtomicUsize::new(0), delay_ms: 150 };
    let cache = LayerCache::default();
    let barrier = std::sync::Barrier::new(4);
    let imgs: Vec<Image> = std::thread::scope(|sc| {
        let hs: Vec<_> = (0..4)
            .map(|i| {
                let (p, src, cache, barrier) = (&p, &src, &cache, &barrier);
                sc.spawn(move || {
                    barrier.wait();
                    frame(p, cid, 0.1 * i as f64, src, None, Some(cache))
                })
            })
            .collect();
        hs.into_iter().map(|h| h.join().unwrap()).collect()
    });
    assert_eq!(src.reads.load(Ordering::SeqCst), 1, "one thread rendered the layer, the others waited for it");
    assert_eq!(cache.inserts(), 1);
    for (i, img) in imgs.iter().enumerate() {
        let fresh = CountingStill { reads: AtomicUsize::new(0), delay_ms: 0 };
        assert_same(img, &frame(&p, cid, 0.1 * i as f64, &fresh, None, None), &format!("thread {i}"));
    }
}

#[test]
fn an_animated_precomp_is_keyed_only_now_and_then() {
    let (mut p, oid, iid) = precomp_scene();
    moving(&mut p.comp_mut(iid).unwrap().layers[0]);
    let cache = LayerCache::default();
    let mut lookups = vec![];
    for f in 0..12 {
        let before = cache.stats();
        frame(&p, oid, f as f64 / 30.0, &NoFootage, None, Some(&cache));
        let after = cache.stats();
        lookups.push(after.hits + after.misses - before.hits - before.misses);
    }
    // Every frame looks up the nested solid (its content is static); the precomp is looked up
    // on the first two frames (both miss), then only every eighth frame.
    assert_eq!(lookups[..2], [2, 2], "{lookups:?}");
    assert_eq!(lookups[2..].iter().filter(|n| **n == 2).count(), 1, "{lookups:?}");
}

/// The blended layer (a cached solid) follows the layer its Blend effect reads (before, its
/// key named that layer but not its content).
#[test]
fn an_effect_reading_another_layer_follows_it() {
    let mut p = project();
    let (cid, c) = comp(&mut p, "C");
    let mut src = solid(&mut p, &c, [0.0, 0.0, 1.0]);
    src.switches.video = false;
    let src_id = src.id.0;
    let mut top = solid(&mut p, &c, [1.0, 0.0, 0.0]);
    let spec = effectcraft_effects::find("ec.channel.blend").unwrap();
    let mut next = p.next_id;
    let mut g = effectcraft_effects::instantiate(spec, &mut Ids(&mut next), spec.name, [40.0, 30.0]);
    p.next_id = next;
    g.prop_mut("blendWithLayer").unwrap().value = Value::Layer(Some(src_id));
    top.props.sub_mut("effects").unwrap().children.push(g.into());
    p.comp_mut(cid).unwrap().layers.push(top);
    p.comp_mut(cid).unwrap().layers.push(src);
    let cache = LayerCache::default();
    let a = frame(&p, cid, 0.0, &NoFootage, None, Some(&cache));
    let LayerSource::Solid { item } = p.comp(cid).unwrap().layers[1].source else { panic!("a solid") };
    if let ItemKind::Solid(s) = &mut p.item_mut(item).unwrap().kind {
        s.color = [0.0, 1.0, 0.0];
    }
    let b = frame(&p, cid, 0.0, &NoFootage, None, Some(&cache));
    let fresh = frame(&p, cid, 0.0, &NoFootage, None, None);
    assert_same(&b, &fresh, "after the read layer changed");
    assert_ne!(a.get(50, 40), b.get(50, 40));
}

/// Motion blur samples the nested layers within the shutter, so the nested time is part of the
/// key (and with an expression in the nested comp, the precomp isn't keyed at all).
#[test]
fn nested_motion_blur_is_keyed_by_time() {
    for expr in [false, true] {
        let (mut p, oid, iid) = precomp_scene();
        let inner = p.comp_mut(iid).unwrap();
        inner.enable_motion_blur = true;
        let l = &mut inner.layers[0];
        l.switches.motion_blur = true;
        moving(l);
        if expr {
            l.props.prop_mut("transform/rotation").unwrap().expr = Some(Expression { text: "time * 10".into(), enabled: true });
        }
        // The outer precomp layer holds still: only the nested content moves.
        p.comp_mut(oid).unwrap().layers[0].props.prop_mut("transform/position").unwrap().keys.clear();
        let host = Outside(AtomicU64::new(5.0f64.to_bits()));
        let cache = LayerCache::default();
        for t in [0.0, 0.5, 1.0, 0.5] {
            let img = frame(&p, oid, t, &NoFootage, Some(&host), Some(&cache));
            assert_same(&img, &frame(&p, oid, t, &NoFootage, Some(&host), None), &format!("expression {expr}, t={t}"));
        }
    }
}
