//! Collapsed 2D effect isolation stays native without changing the CPU drawing order.
use crate::tests::{self, Pattern, Scene, n, set, v3};
use effectcraft_color::{BlendMode, Label};
use effectcraft_keyframe::Keyframe;
use effectcraft_project::{BitDepth, Comp, ItemKind, LayerSource, MatteKind, TrackMatte, build};
use effectcraft_render::{Backend, RenderOpts, Renderer};
use effectcraft_time::{FrameRate, Tick};

fn scene(depth: BitDepth, linear: bool, motion: bool, matte: bool, cpu_effect: bool) -> Scene {
    let mut s = Scene::new(depth);
    s.p.settings.blend_linear = linear;
    let bg = s.solid([0.13, 0.22, 0.31], 97, 61);
    s.push(bg);
    let inner = Comp::new(43, 31, FrameRate::FPS_30, Tick::from_seconds_f64(2.0));
    let iid = s.p.add_item("generated", Label::Sandstone, None, ItemKind::Comp(inner.clone().into()));
    let mut child = Scene { p: std::mem::take(&mut s.p), cid: iid, comp: inner };
    let mut a = child.solid([0.8, 0.3, 0.1], 37, 23);
    set(&mut a, "transform/position", v3(16.0, 13.0));
    child.push(a);
    s.p = child.p;
    let mut parent = build::layer(&mut s.p, &s.comp, "generated", LayerSource::Comp { item: iid }, (43, 31), None);
    parent.switches.collapse = true;
    set(&mut parent, "transform/position", v3(46.0, 29.0));
    set(&mut parent, "transform/scale", v3(130.0, 110.0));
    set(&mut parent, "transform/rotation", n(13.0));
    set(&mut parent, "transform/opacity", n(73.0));
    s.effect(&mut parent, "ec.perspective.dropshadow", &[("distance", n(4.0)), ("softness", n(3.0)), ("opacity", n(61.0))]);
    if cpu_effect {
        s.effect(&mut parent, "ec.generate.fractal", &[]);
    }
    if motion {
        s.p.comp_mut(s.cid).unwrap().enable_motion_blur = true;
        parent.switches.motion_blur = true;
        parent.props.prop_mut("transform/position").unwrap().keys =
            vec![Keyframe::new(Tick::ZERO, v3(38.0, 25.0)), Keyframe::new(Tick::from_seconds_f64(1.0), v3(54.0, 34.0))];
    }
    if matte {
        let mut m = s.solid([0.7, 0.7, 0.7], 52, 61);
        set(&mut m, "transform/position", v3(30.0, 30.0));
        let id = s.push(m);
        parent.track_matte = Some(TrackMatte { layer: id, kind: MatteKind::LumaInverted });
        parent.blend_mode = BlendMode::Multiply;
        parent.preserve_transparency = true;
    }
    s.push(parent);
    s
}

#[test]
fn collapsed_effects_native_zero_readback_and_cpu_oracle() {
    let Some(g) = tests::gpu() else {
        assert!(std::env::var_os("EFFECTCRAFT_REQUIRE_GPU").is_none());
        return;
    };
    for depth in [BitDepth::Bpc8, BitDepth::Bpc16, BitDepth::Bpc32] {
        for linear in [false, true] {
            for scale in [1.0, 0.5] {
                for motion in [false, true] {
                    for matte in [false, true] {
                        let s = scene(depth, linear, motion, matte, false);
                        let opts = RenderOpts { scale, motion_blur: motion, ..Default::default() };
                        let t = Tick::from_seconds_f64(0.5);
                        let mut r = Renderer::new(&s.p, &Pattern, RenderOpts { backend: Backend::Gpu, ..opts });
                        r.accel = Some(g);
                        let before = g.ctx.transfer_stats();
                        g.within_memory(|| g.render_display(&r, s.cid, t)).unwrap().unwrap();
                        assert_eq!(g.ctx.transfer_stats().readbacks, before.readbacks, "collapsed isolation must stay resident");
                        let got = g.render(&r, s.cid, t).unwrap();
                        let cpu = Renderer::new(&s.p, &Pattern, RenderOpts { backend: Backend::Cpu, ..opts }).comp_frame_cpu(s.cid, t);
                        for (a, b) in got.data.iter().flatten().zip(cpu.data.iter().flatten()) {
                            assert!(
                                (a - b).abs() <= tests::tolerance(depth),
                                "{depth:?} linear={linear} scale={scale} motion={motion} matte={matte}: {a} != {b}"
                            );
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn collapsed_effects_cpu_only_effect_keeps_fallback_pixels() {
    let Some(g) = tests::gpu() else {
        return;
    };
    let s = scene(BitDepth::Bpc32, false, false, false, true);
    let opts = RenderOpts::default();
    let mut r = Renderer::new(&s.p, &Pattern, opts);
    r.accel = Some(g);
    let before = g.ctx.transfer_stats();
    let got = g.render(&r, s.cid, Tick::ZERO).unwrap();
    assert!(g.ctx.transfer_stats().readbacks > before.readbacks + 1);
    let cpu = Renderer::new(&s.p, &Pattern, RenderOpts { backend: Backend::Cpu, ..opts }).comp_frame_cpu(s.cid, Tick::ZERO);
    for (a, b) in got.data.iter().flatten().zip(cpu.data.iter().flatten()) {
        assert!((a - b).abs() <= 1e-3);
    }
}
