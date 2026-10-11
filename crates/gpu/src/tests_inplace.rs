//! Layers drawn onto the canvas in place over their bounds (`ops::warp_q_in`) give bit for bit
//! the pixels of one full-canvas pass per layer (`ops::warp_q`), inside the bounds and out;
//! stencil modes, layers covering most of the canvas and a canvas not yet quantised take the
//! full pass.
use effectcraft_color::BlendMode::{self, Add, Difference, Dissolve, Multiply, Normal, Overlay, Screen, StencilAlpha};
use effectcraft_geom::{Mat3, vec2};
use effectcraft_raster::Sampling::{self, Bicubic, Bilinear, Nearest};

use crate::context::Enc;
use crate::ops;
use crate::tests;

struct Step {
    m: Mat3,
    sampling: Sampling,
    mode: BlendMode,
    opacity: f32,
    accumulate: Option<f32>,
    levels: Option<f32>,
    /// Drawn on the canvas's own texture (or nothing drawn at all).
    in_place: bool,
}

fn step(m: Mat3, sampling: Sampling, mode: BlendMode, opacity: f32, levels: Option<f32>, in_place: bool) -> Step {
    Step { m, sampling, mode, opacity, accumulate: None, levels, in_place }
}

/// A 40 × 30 layer centred at (`x`, `y`), rotated and scaled.
fn at(x: f64, y: f64, deg: f64, s: f64) -> Mat3 {
    Mat3::translate(vec2(x, y)) * Mat3::rotate_deg(deg) * Mat3::scale(vec2(s, s)) * Mat3::translate(vec2(-20.0, -15.0))
}

#[test]
fn in_place_layers_match_full_canvas_passes() {
    let Some(g) = tests::gpu() else {
        assert!(std::env::var_os("EFFECTCRAFT_REQUIRE_GPU").is_none());
        return;
    };
    let (w, h) = (160, 120);
    let bg = g.ctx.upload_image(&tests::pattern(3, w, h)).unwrap();
    let layer = g.ctx.upload_image(&tests::pattern(7, 40, 30)).unwrap();
    let q = Some(255.0);
    let steps = [
        // Partial bounds, then hanging off the top-left and the bottom-right edges.
        step(at(50.0, 40.0, 10.0, 1.0), Bilinear, Normal, 0.8, None, true),
        step(at(5.0, 3.0, 30.0, 1.2), Bicubic, Multiply, 1.0, None, true),
        step(at(155.0, 118.0, -20.0, 1.0), Nearest, Screen, 1.0, None, true),
        // Minified (pre-filtered levels), dissolve noise (absolute pixel coordinates).
        step(at(80.0, 60.0, 45.0, 0.2), Bilinear, Normal, 1.0, None, true),
        step(at(70.0, 50.0, 0.0, 0.9), Bilinear, Dissolve, 0.6, None, true),
        // Entirely off the canvas: nothing drawn, the canvas kept.
        step(at(500.0, 500.0, 0.0, 1.0), Bilinear, Normal, 1.0, None, true),
        // Stencil clears outside its bounds; a layer covering the canvas: full passes.
        step(at(60.0, 60.0, 0.0, 1.0), Bilinear, StencilAlpha, 1.0, None, false),
        step(at(80.0, 60.0, 0.0, 5.0), Bilinear, Normal, 0.7, None, false),
        // 8 bpc: the first pass quantises the whole canvas, later ones are in place.
        step(at(40.0, 40.0, 15.0, 1.0), Bilinear, Add, 0.9, q, false),
        step(at(90.0, 70.0, -15.0, 1.0), Bicubic, Normal, 0.9, q, true),
        step(at(30.0, 80.0, 5.0, 0.7), Bilinear, Overlay, 1.0, q, true),
        step(at(100.0, 30.0, 0.0, 0.3), Bicubic, Difference, 1.0, q, true),
        step(at(120.0, 90.0, 0.0, 1.0), Bilinear, Normal, 0.5, None, true),
        // A motion-blur accumulate sample.
        Step { accumulate: Some(0.25), ..step(at(60.0, 70.0, 20.0, 1.0), Bilinear, Normal, 1.0, None, true) },
    ];
    let mut e = Enc::new(&g.ctx);
    let mut full = bg.clone();
    let mut canvas = e.scratch(w, h);
    e.copy_into(&bg, &canvas, 0, 0);
    let mut shots = Vec::new();
    for (i, s) in steps.iter().enumerate() {
        assert!(e.owns(&canvas), "step {i}: the canvas is the encoder's own");
        full = ops::warp_q(&mut e, &full, &layer, &s.m, s.sampling, s.mode, s.opacity, 5, s.accumulate, s.levels);
        let before = canvas.texture.clone();
        ops::warp_q_in(&mut e, &mut canvas, &layer, &s.m, s.sampling, s.mode, s.opacity, 5, s.accumulate, s.levels);
        assert_eq!(canvas.texture == before, s.in_place, "step {i}: in place");
        assert_eq!(canvas.levels, full.levels, "step {i}");
        if let Some(l) = s.levels {
            full = ops::quantize(&mut e, &full, l);
            canvas = ops::quantize(&mut e, &canvas, l);
        }
        // Snapshot by copy: a clone would share the canvas (no longer the encoder's own).
        let shot = e.scratch(w, h);
        e.copy_into(&canvas, &shot, 0, 0);
        shots.push((full.clone(), shot));
    }
    let bits = |img: &effectcraft_raster::Image| img.data.iter().flatten().map(|v| v.to_bits()).collect::<Vec<u32>>();
    for (i, (full, shot)) in shots.iter().enumerate() {
        let (full, shot) = (e.download(full).unwrap(), e.download(shot).unwrap());
        assert!(bits(&full) == bits(&shot), "step {i}: {:?}", tests::diff(&full, &shot, 0.0));
    }
    let start = e.download(&bg).unwrap();
    assert!(tests::diff(&start, &e.download(&full).unwrap(), 1e-3).over > 1000, "the layers draw");
}
