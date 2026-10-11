//! GPU Deep Glow (`fx_deepglow`) vs the CPU effect (the oracle): threshold, radius (including
//! the reduced working resolution past 512 px), exposure, tint, chromatic aberration, the
//! display and linear models, Glow Input view, and the Mask Layer, at 8 and 32 bpc.

use effectcraft_keyframe::Value;
use effectcraft_project::BitDepth;
use effectcraft_render::RenderOpts;
use effectcraft_time::Tick;

use crate::tests::{Scene, c, check, compare_at, effect_case, n, opts, set};

fn e(v: u32) -> Value {
    Value::Enum(v)
}

fn b(v: bool) -> Value {
    Value::Bool(v)
}

const ID: &str = "ec.stylize.deepglow";

#[test]
fn deep_glow_matches_cpu() {
    // Linear model (Auto Detect Gamma), Screen, Unmult, Dither, Downsample 80 %.
    effect_case(ID, &[("PEDG-0001", n(20.0)), ("PEDG-0004", n(30.0)), ("PEDG-0005", n(10.0))]);
    // Display model, Add, exposure, Overlay tint, chromatic aberration, aspect, no Unmult.
    effect_case(
        ID,
        &[
            ("PEDG-0001", n(14.0)),
            ("PEDG-0043", b(false)),
            ("PEDG-0008", e(1)),
            ("PEDG-0002", n(2.5)),
            ("PEDG-0037", b(true)),
            ("PEDG-0039", e(1)),
            ("PEDG-0041", n(70.0)),
            ("PEDG-0013", b(true)),
            ("PEDG-0015", n(0.8)),
            ("PEDG-0010", n(0.6)),
            ("PEDG-0025", b(false)),
            ("PEDG-0031", b(false)),
        ],
    );
    // Soft Light tint, spread, strong downsample, source opacity, monochrome dither, expansion.
    effect_case(
        ID,
        &[
            ("PEDG-0001", n(24.0)),
            ("PEDG-0004", n(55.0)),
            ("PEDG-0037", b(true)),
            ("PEDG-0038", c(0.8, 0.3, 0.6)),
            ("PEDG-0039", e(2)),
            ("PEDG-0011", n(70.0)),
            ("PEDG-0019", n(30.0)),
            ("PEDG-0024", n(60.0)),
            ("PEDG-0032", b(true)),
            ("PEDG-0055", n(2.0)),
            ("PEDG-0084", e(1)),
        ],
    );
    // Past 512 px: the reduced working resolution (no edge expansion, to stay small).
    effect_case(ID, &[("PEDG-0001", n(1100.0)), ("PEDG-0055", n(0.0)), ("PEDG-0013", b(true))]);
    // Glow Input view.
    effect_case(ID, &[("PEDG-0001", n(16.0)), ("PEDG-0004", n(40.0)), ("PEDG-0023", e(0))]);
}

/// The Mask Layer (fetched through the effect host, fitted on the CPU), alpha and inverted luma.
#[test]
fn deep_glow_mask_layer_matches_cpu() {
    for depth in [BitDepth::Bpc8, BitDepth::Bpc32] {
        for (luma, invert) in [(0, false), (1, true)] {
            let mut s = Scene::new(depth);
            let bg = s.solid([0.15, 0.1, 0.2], 97, 61);
            s.push(bg);
            let mask = s.footage(40, 30);
            let mid = s.push(mask);
            let mut l = s.footage(70, 44);
            s.effect(
                &mut l,
                ID,
                &[("PEDG-0001", n(18.0)), ("PEDG-0004", n(20.0)), ("PEDG-0056", Value::Layer(Some(mid.0))), ("PEDG-0057", e(luma)), ("PEDG-0059", b(invert))],
            );
            set(&mut l, "transform/rotation", Value::Scalar(8.0));
            s.push(l);
            let label = format!("deep glow mask {luma} {invert} {depth:?}");
            check(&label, compare_at(&s, opts(), Tick::ZERO), 0.0);
            check(&format!("{label} half"), compare_at(&s, RenderOpts { scale: 0.5, ..opts() }, Tick::ZERO), 0.0);
        }
    }
}
