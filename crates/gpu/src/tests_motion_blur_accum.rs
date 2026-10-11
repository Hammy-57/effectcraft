//! Motion-blur sub-samples summed in one pass per minification level (`ops::warp_accumulate`)
//! give the same pixels as one accumulate warp per sample, and the same frames as the CPU.
use crate::context::Enc;
use crate::ops;
use crate::tests::{self, Scene, check, compare_at, n, v3};
use effectcraft_color::BlendMode;
use effectcraft_geom::{Mat3, vec2};
use effectcraft_keyframe::Keyframe;
use effectcraft_project::{BitDepth, MatteKind, TrackMatte};
use effectcraft_raster::Sampling;
use effectcraft_render::RenderOpts;
use effectcraft_time::Tick;

/// Shutter-like matrix runs: rotation + partial off-canvas travel, a shrink that crosses
/// minification levels mid-run, samples wholly off the canvas, and a perspective sample.
fn runs() -> Vec<(&'static str, Vec<Mat3>)> {
    let place =
        |x: f64, y: f64, deg: f64, s: f64| Mat3::translate(vec2(x, y)) * Mat3::rotate_deg(deg) * Mat3::scale(vec2(s, s)) * Mat3::translate(vec2(-60.0, -45.0));
    let travel = (0..16).map(|i| place(-30.0 + i as f64 * 9.0, 50.0 + i as f64 * 1.5, i as f64 * 4.0, 1.1)).collect();
    let shrink = (0..24).map(|i| place(80.0, 60.0, 10.0 + i as f64, 0.62 - i as f64 * 0.02)).collect();
    let mut off: Vec<Mat3> = (0..20).map(|i| place(-200.0 + i as f64 * 20.0, 60.0, 0.0, 0.9)).collect();
    off.push(place(5000.0, -4000.0, 0.0, 1.0));
    let mut persp = place(70.0, 50.0, 5.0, 0.8);
    persp.0[2] = [0.0004, -0.0003, 1.0];
    let mixed = vec![place(40.0, 40.0, 0.0, 1.0), persp, place(60.0, 52.0, 3.0, 0.3), place(64.0, 54.0, 6.0, 0.29)];
    vec![("travel", travel), ("shrink", shrink), ("off canvas", off), ("mixed", mixed)]
}

#[test]
fn one_pass_accumulation_matches_per_sample_warps() {
    let Some(g) = tests::gpu() else {
        assert!(std::env::var_os("EFFECTCRAFT_REQUIRE_GPU").is_none());
        return;
    };
    let src = g.ctx.upload_image(&tests::pattern(7, 120, 90)).unwrap();
    let base = g.ctx.upload_image(&tests::pattern(3, 160, 120)).unwrap();
    assert!(Enc::new(&g.ctx).has_kernel("warp_accum"), "the one-pass kernel builds");
    for sampling in [Sampling::Nearest, Sampling::Bilinear, Sampling::Bicubic] {
        for (label, ms) in runs() {
            for dst in [&base, &g.ctx.upload_image(&effectcraft_raster::Image::new(160, 120)).unwrap()] {
                let k = 1.0 / ms.len() as f32;
                let mut e = Enc::new(&g.ctx);
                let old = ms.iter().fold(dst.clone(), |acc, m| ops::warp(&mut e, &acc, &src, m, sampling, BlendMode::Normal, 1.0, 0, Some(k)));
                let new = ops::warp_accumulate(&mut e, dst, &src, &ms, sampling, k);
                let (old, new) = (e.download(&old).unwrap(), e.download(&new).unwrap());
                let d = tests::diff(&old, &new, 1e-6);
                assert_eq!(d.over, 0, "{label} {sampling:?}: {d:?}");
                let before = e.download(dst).unwrap();
                assert!(tests::diff(&before, &new, 1e-3).over > 100, "{label} {sampling:?}: the samples draw");
            }
        }
    }
}

/// A motion-blurred footage layer (rotation, scale, partly off the canvas) over a background,
/// optionally through a track matte, and a shrinking one (minification levels).
fn scene(depth: BitDepth, matte: bool) -> Scene {
    let mut s = Scene::new(depth);
    s.p.comp_mut(s.cid).unwrap().enable_motion_blur = true;
    s.comp.enable_motion_blur = true;
    let bg = s.solid([0.12, 0.2, 0.3], 97, 61);
    s.push(bg);
    let matte_id = matte.then(|| {
        let mut m = s.solid([0.8, 0.8, 0.8], 50, 40);
        tests::set(&mut m, "transform/position", v3(40.0, 30.0));
        m.switches.motion_blur = true;
        m.props.prop_mut("transform/rotation").unwrap().keys = vec![Keyframe::new(Tick::ZERO, n(0.0)), Keyframe::new(Tick::from_seconds_f64(1.0), n(90.0))];
        s.push(m)
    });
    let mut l = s.footage(40, 36);
    l.switches.motion_blur = true;
    l.props.prop_mut("transform/position").unwrap().keys =
        vec![Keyframe::new(Tick::ZERO, v3(-25.0, 20.0)), Keyframe::new(Tick::from_seconds_f64(1.0), v3(110.0, 45.0))];
    l.props.prop_mut("transform/rotation").unwrap().keys = vec![Keyframe::new(Tick::ZERO, n(0.0)), Keyframe::new(Tick::from_seconds_f64(1.0), n(240.0))];
    l.props.prop_mut("transform/scale").unwrap().keys =
        vec![Keyframe::new(Tick::ZERO, v3(60.0, 60.0)), Keyframe::new(Tick::from_seconds_f64(1.0), v3(150.0, 120.0))];
    if let Some(id) = matte_id {
        l.track_matte = Some(TrackMatte { layer: id, kind: MatteKind::Alpha });
    }
    s.push(l);
    let mut small = s.footage(120, 120);
    small.switches.motion_blur = true;
    small.blend_mode = BlendMode::Screen;
    small.props.prop_mut("transform/position").unwrap().keys =
        vec![Keyframe::new(Tick::ZERO, v3(70.0, 15.0)), Keyframe::new(Tick::from_seconds_f64(1.0), v3(20.0, 50.0))];
    small.props.prop_mut("transform/scale").unwrap().keys =
        vec![Keyframe::new(Tick::ZERO, v3(40.0, 40.0)), Keyframe::new(Tick::from_seconds_f64(1.0), v3(8.0, 8.0))];
    s.push(small);
    s
}

#[test]
fn motion_blur_one_pass_matches_the_cpu() {
    if tests::gpu().is_none() {
        assert!(std::env::var_os("EFFECTCRAFT_REQUIRE_GPU").is_none());
        return;
    }
    for depth in [BitDepth::Bpc8, BitDepth::Bpc16, BitDepth::Bpc32] {
        for scale in [1.0, 0.5] {
            for matte in [false, true] {
                for t in [0.1, 0.45] {
                    let s = scene(depth, matte);
                    let opts = RenderOpts { scale, ..tests::opts() };
                    check(&format!("{depth:?} scale={scale} matte={matte} t={t}"), compare_at(&s, opts, Tick::from_seconds_f64(t)), 0.0);
                }
            }
        }
    }
}
