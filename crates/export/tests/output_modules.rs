//! A Render Queue item with several output modules exports each module as a job of its own,
//! back to back. A module's job continues from the layer cache of the module before it when
//! both render the same frames (G5): the files must be the bytes of each module exported alone.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use effectcraft_color::Label;
use effectcraft_export::render_queue::{OutputFormat, OutputModule, RenderSettings, TimeSpan};
use effectcraft_export::{Job, export};
use effectcraft_keyframe::{Keyframe, Value};
use effectcraft_media::MediaPool;
use effectcraft_project::build::{self, Ids};
use effectcraft_project::{Comp, ItemId, ItemKind, Layer, LayerSource, Project, Solid};
use effectcraft_time::{FrameRate, Tick};

fn solid(p: &mut Project, comp: &Comp, color: [f32; 3], w: u32, h: u32) -> Layer {
    let sid = p.add_item("Solid", Label::Red, None, ItemKind::Solid(Solid { color, width: w, height: h, pixel_aspect: 1.0 }));
    build::layer(p, comp, "Solid", LayerSource::Solid { item: sid }, (w, h), None)
}

/// 96×64 @ 10 fps, 12 frames: a blurred static solid (its buffer is cached) under a smaller solid
/// moving across.
fn project() -> (Project, ItemId) {
    let mut p = Project::default();
    let mut comp = Comp::new(96, 64, FrameRate::new(10, 1), Tick::from_seconds_f64(1.2));
    comp.background = [0.1, 0.2, 0.4];
    let mut bg = solid(&mut p, &comp, [0.9, 0.6, 0.1], 64, 40);
    let spec = effectcraft_effects::find("ec.blur.gaussian").expect("gaussian blur");
    let mut next = p.next_id;
    let mut fx = effectcraft_effects::instantiate(spec, &mut Ids(&mut next), spec.name, [64.0, 40.0]);
    p.next_id = next;
    fx.prop_mut("blurriness").expect("blurriness").value = Value::Scalar(12.0);
    bg.props.sub_mut("effects").expect("effects").children.push(fx.into());
    let mut fg = solid(&mut p, &comp, [0.2, 0.9, 0.3], 16, 16);
    fg.props.prop_mut("transform/position").expect("position").keys =
        vec![Keyframe::new(Tick::ZERO, Value::Vec3([0.0, 20.0, 0.0])), Keyframe::new(Tick::from_seconds_f64(1.2), Value::Vec3([96.0, 44.0, 0.0]))];
    comp.layers.push(fg);
    comp.layers.push(bg);
    let cid = p.add_item("Comp", Label::Sandstone, None, ItemKind::Comp(comp.into()));
    (p, cid)
}

/// Export one module into memory with `footage`: the files it wrote, by path.
fn run(p: &Project, cid: ItemId, footage: &MediaPool, om: &OutputModule, path: &str) -> BTreeMap<String, Vec<u8>> {
    let settings = RenderSettings { time_span: TimeSpan::LengthOfComp, ..Default::default() };
    let files = Arc::new(Mutex::new(BTreeMap::new()));
    let out = files.clone();
    let sink = move |path: &str, data: Vec<u8>| {
        out.lock().unwrap().insert(path.to_string(), data);
    };
    let job = Job {
        project: p,
        footage,
        expr: None,
        accel: None,
        comp: cid,
        settings: &settings,
        output: om,
        path,
        sink: Some(&sink),
        options: Default::default(),
        nested_switches: true,
    };
    let r = export(&job, &mut |_| true);
    assert!(r.is_ok(), "{path}: {r:?}");
    std::mem::take(&mut *files.lock().unwrap())
}

#[test]
fn every_output_module_writes_the_bytes_it_writes_alone() {
    let (p, cid) = project();
    let modules = [
        (OutputModule::for_format(OutputFormat::H264), "/mem/out.mp4"),
        (OutputModule::for_format(OutputFormat::PngSequence), "/mem/out_[#####].png"),
        (OutputModule::for_format(OutputFormat::ProRes), "/mem/out.mov"),
    ];
    // Alone: each with a footage pool of its own, so no job continues from another's cache.
    let pools: Vec<MediaPool> = modules.iter().map(|_| MediaPool::new()).collect();
    let alone: Vec<_> = modules.iter().zip(&pools).map(|((om, path), pool)| run(&p, cid, pool, om, path)).collect();
    assert_eq!(alone[1].len(), 12, "one PNG per frame");
    // As one item: the same pool, back to back (modules after the first find the cache warm).
    let pool = MediaPool::new();
    for ((om, path), alone) in modules.iter().zip(&alone) {
        let item = run(&p, cid, &pool, om, path);
        assert!(!item.is_empty());
        assert!(item == *alone, "{path} differs from the module exported alone");
    }
}
