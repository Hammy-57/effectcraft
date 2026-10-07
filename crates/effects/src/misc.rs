//! Blur & Sharpen, Stylize, Perspective, Channel, Noise and Transition effects.

use effectcraft_color::luminance;
use effectcraft_keyframe::Value;
use effectcraft_project::ParamUi;
use effectcraft_raster::{Image, gaussian_blur, hash_noise, resample};
use rayon::prelude::*;

use crate::{Buf, EffectCtx, EffectSpec, col, num, p, popup, slider};

fn spec(id: &'static str, name: &'static str, category: &'static str, params: Vec<crate::ParamSpec>, render: crate::RenderFn) -> EffectSpec {
    EffectSpec { id, name, category, params, render, gpu: false, float: true }
}

/// AE's Blurriness is roughly a radius; our Gaussian uses sigma = blurriness / 2.
fn blur_sigma(blurriness: f64) -> f64 {
    blurriness.max(0.0) * 0.5
}

fn dims_xy(dim: u32) -> (f64, f64) {
    match dim {
        1 => (1.0, 0.0),
        2 => (0.0, 1.0),
        _ => (1.0, 1.0),
    }
}

fn gaussian(ctx: &EffectCtx, mut b: Buf) -> Buf {
    let s = blur_sigma(ctx.params.f("blurriness")) * b.scale;
    if s <= 0.0 {
        return b;
    }
    let (kx, ky) = dims_xy(ctx.params.e("dimensions"));
    let repeat = ctx.params.b("repeatEdge");
    if !repeat && !ctx.adjustment {
        b.pad((s * 3.0).ceil() as u32);
    }
    b.img = gaussian_blur(&b.img, s * kx, s * ky, repeat || ctx.adjustment);
    b
}

fn box_blur(ctx: &EffectCtx, mut b: Buf) -> Buf {
    let r = (ctx.params.f("radius") * b.scale).round().max(0.0) as usize;
    if r == 0 {
        return b;
    }
    let it = ctx.params.f("iterations").round().clamp(1.0, 50.0) as usize;
    let (kx, ky) = dims_xy(ctx.params.e("dimensions"));
    let repeat = ctx.params.b("repeatEdge");
    if !repeat && !ctx.adjustment {
        b.pad((r * it) as u32 + 1);
    }
    b.img = effectcraft_raster::box_blur(&b.img, r * kx as usize, r * ky as usize, it, repeat || ctx.adjustment);
    b
}

fn directional(ctx: &EffectCtx, mut b: Buf) -> Buf {
    let len = ctx.params.f("length") * b.scale;
    if len < 0.5 {
        return b;
    }
    if !ctx.adjustment {
        b.pad(len.ceil() as u32 + 1);
    }
    b.img = effectcraft_raster::directional_blur(&b.img, ctx.params.f("direction"), len);
    b
}

fn radial(ctx: &EffectCtx, mut b: Buf) -> Buf {
    let amt = ctx.params.f("amount");
    let zoom = ctx.params.e("type") == 1;
    let c = b.to_px(ctx.params.v2("center"));
    b.img = effectcraft_raster::radial_blur(&b.img, c, if zoom { amt / 100.0 } else { amt }, zoom);
    b
}

fn unsharp_core(img: &Image, amount: f32, sigma: f64, threshold: f32) -> Image {
    let blurred = gaussian_blur(img, sigma, sigma, true);
    let mut out = img.clone();
    out.data.par_iter_mut().zip(blurred.data.par_iter()).for_each(|(o, bl)| {
        for c in 0..3 {
            let d = o[c] - bl[c];
            if d.abs() * 255.0 >= threshold {
                o[c] = (o[c] + d * amount).max(0.0);
            }
        }
    });
    out
}

fn sharpen(ctx: &EffectCtx, mut b: Buf) -> Buf {
    let a = ctx.params.f("amount") as f32 / 50.0;
    b.img = unsharp_core(&b.img, a, 1.0 * b.scale, 0.0);
    b
}

fn unsharp(ctx: &EffectCtx, mut b: Buf) -> Buf {
    let a = ctx.params.f("amount") as f32 / 100.0;
    b.img = unsharp_core(&b.img, a, (ctx.params.f("radius") * b.scale).max(0.1), ctx.params.f("threshold") as f32);
    b
}

/// Position (0 = Color A, 1 = Color B) in Glow's A & B gradient for brightness `l`: `loops`
/// cycles starting at `phase` (revolutions), shaped by Color Looping (0 Sawtooth A>B,
/// 1 Sawtooth B>A, 2 Triangle A>B>A, 3 Triangle B>A>B) and biased so `mid` maps to the middle.
pub fn glow_ab_t(l: f32, looping: u32, loops: f32, phase: f32, mid: f32) -> f32 {
    let u = l.clamp(0.0, 1.0) * loops.max(0.0) + phase;
    let mut t = u - u.floor();
    if u > 0.0 && t <= 0.0 {
        t = 1.0;
    }
    let tri = 1.0 - (2.0 * t - 1.0).abs();
    let t = match looping {
        0 => t,
        1 => 1.0 - t,
        3 => 1.0 - tri,
        _ => tri,
    };
    let m = mid.clamp(0.01, 0.99);
    t.max(0.0).powf(0.5f32.ln() / m.ln())
}

/// Glow Operation options (blend modes the glow is applied with).
pub const GLOW_OPERATIONS: [&str; 17] = [
    "Normal",
    "Add",
    "Multiply",
    "Screen",
    "Overlay",
    "Soft Light",
    "Hard Light",
    "Color Dodge",
    "Color Burn",
    "Darken",
    "Lighten",
    "Difference",
    "Exclusion",
    "Hue",
    "Saturation",
    "Color",
    "Luminosity",
];

/// Glow's Glow Operation as a blend mode (Add for projects saved before it existed).
pub fn glow_operation(ctx: &EffectCtx) -> effectcraft_color::BlendMode {
    let name = GLOW_OPERATIONS.get(ctx.params.get("glowOperation").map(|v| v.as_enum()).unwrap_or(1) as usize).copied().unwrap_or("Add");
    effectcraft_color::BlendMode::ALL.iter().copied().find(|m| m.label() == name).unwrap_or(effectcraft_color::BlendMode::Add)
}

/// Glow's Arbitrary Map: red, green and blue curves of the glow brightness, as Curves point
/// lists `x,y …` separated by `|` (an empty or missing curve is the identity).
fn glow_map(s: &str) -> Vec<Option<crate::Curve>> {
    let mut parts = s.split('|');
    (0..3).map(|_| parts.next().and_then(crate::Curve::parse)).collect()
}

fn glow(ctx: &EffectCtx, mut b: Buf) -> Buf {
    let thr = ctx.params.f("threshold") as f32 / 100.0;
    let radius = ctx.params.f("radius") * b.scale;
    let intensity = ctx.params.f("intensity") as f32;
    let alpha_based = ctx.params.e("based") == 0;
    let use_colors = ctx.params.e("colors") == 1;
    let arbitrary = (ctx.params.e("colors") == 2).then(|| glow_map(ctx.params.s("arbitraryMap")));
    let glow_op = glow_operation(ctx);
    let looping = ctx.params.e("colorLooping");
    let loops = ctx.params.f("colorLoops") as f32;
    let phase = (ctx.params.f("colorPhase") / 360.0) as f32;
    let mid = ctx.params.f("abMidpoint") as f32 / 100.0;
    let ca = ctx.params.color("colorA");
    let cb = ctx.params.color("colorB");
    let (kx, ky) = dims_xy(ctx.params.e("glowDimensions"));
    if !ctx.adjustment {
        b.pad((radius * 1.5).ceil() as u32 + 2);
    }
    // Bright pass.
    let mut bright = b.img.clone();
    bright.data.par_iter_mut().for_each(|px| {
        let a = px[3];
        if a <= 0.0 {
            *px = [0.0; 4];
            return;
        }
        // Glow Based On: brightness from the colours, or from the alpha channel.
        let l = if alpha_based { a } else { luminance(px[0] / a, px[1] / a, px[2] / a) };
        let k = ((l - thr) / (1.0 - thr).max(1e-3)).clamp(0.0, 1.0);
        if let Some(map) = &arbitrary {
            let t = glow_ab_t(l, looping, loops, phase, mid);
            let c = map.iter().map(|cv| cv.as_ref().map_or(t, |cv| cv.eval(t))).collect::<Vec<f32>>();
            *px = [c[0] * k * a, c[1] * k * a, c[2] * k * a, k * a];
        } else if use_colors {
            let t = glow_ab_t(l, looping, loops, phase, mid);
            let c = [ca[0] + (cb[0] - ca[0]) * t, ca[1] + (cb[1] - ca[1]) * t, ca[2] + (cb[2] - ca[2]) * t];
            *px = [c[0] * k * a, c[1] * k * a, c[2] * k * a, k * a];
        } else {
            *px = px.map(|v| v * k);
        }
    });
    let s = (radius / 2.0).max(0.5);
    let blurred = gaussian_blur(&bright, s * kx, s * ky, false);
    let mode = ctx.params.e("operation");
    b.img.data.par_iter_mut().zip(blurred.data.par_iter()).for_each(|(o, g)| {
        let g = g.map(|v| v * intensity);
        match mode {
            // Behind: the glow shows only where the original is transparent.
            1 => {
                let k = 1.0 - o[3];
                for c in 0..4 {
                    o[c] += g[c] * k;
                }
            }
            // None: the glow alone.
            2 => *o = [g[0], g[1], g[2], g[3]],
            // On Top with another Glow Operation: that blend mode.
            _ if glow_op != effectcraft_color::BlendMode::Add => *o = effectcraft_color::blend_pixel(glow_op, *o, g, 0.0),
            // On Top: add (screen-like add on premultiplied colour; alpha grows by the glow's alpha).
            _ => {
                for c in 0..3 {
                    o[c] += g[c];
                }
                o[3] = (o[3] + g[3] * (1.0 - o[3])).min(1.0);
            }
        }
        o[3] = o[3].clamp(0.0, 1.0);
    });
    b
}

/// Editable, clean-room approximation of Plugin Everything Deep Glow v1. The public product
/// description specifies a wide inverse-square-like falloff; three Gaussian scales approximate
/// that profile without depending on the third-party plug-in.
fn deep_glow(ctx: &EffectCtx, mut b: Buf) -> Buf {
    let radius = (ctx.params.f("PEDG-0001") * b.scale).clamp(0.0, 512.0);
    if !radius.is_finite() || radius < 0.5 {
        return b;
    }
    // Mode 2 is the expanded transparent-edge setting found in every SceneYard instance.
    let edge = ctx.params.e("PEDG-0055");
    let pad = if edge == 0 || ctx.adjustment { 0 } else { (radius * if ctx.params.e("PEDG-0084") == 1 { 1.5 } else { 1.0 }).ceil() as u32 };
    // ponytail: bound full-image padding; tile the glow for larger frames or radii.
    let expanded_w = b.img.width.saturating_add(pad.saturating_mul(2));
    let expanded_h = b.img.height.saturating_add(pad.saturating_mul(2));
    if u64::from(expanded_w) * u64::from(expanded_h) > 32_000_000 {
        return b;
    }
    if pad > 0 {
        b.pad(pad);
    }
    let threshold = (ctx.params.f("PEDG-0004") / 100.0).max(0.0) as f32;
    let smooth = (ctx.params.f("PEDG-0005") / 100.0).max(0.0) as f32;
    let mut source = b.img.clone();
    source.data.par_iter_mut().for_each(|px| {
        let a = px[3];
        let l = if a > 0.0 { luminance(px[0] / a, px[1] / a, px[2] / a) } else { 0.0 };
        let k = if smooth > 0.0 {
            ((l - threshold) / smooth).clamp(0.0, 1.0)
        } else if l >= threshold {
            1.0
        } else {
            0.0
        };
        for v in px.iter_mut() {
            *v *= k;
        }
    });
    if let Some(layer) = ctx.layer_param("PEDG-0056", true) {
        let mask = crate::util::fit_layer(ctx, &b, &layer, false);
        let luma = ctx.params.e("PEDG-0057") == 1;
        let invert = ctx.params.b("PEDG-0059");
        source.data.par_iter_mut().zip(mask.data.par_iter()).for_each(|(px, m)| {
            let mut amount = if luma && m[3] > 0.0 { luminance(m[0] / m[3], m[1] / m[3], m[2] / m[3]) * m[3] } else { m[3] };
            if invert {
                amount = 1.0 - amount;
            }
            for channel in px.iter_mut() {
                *channel *= amount.clamp(0.0, 1.0);
            }
        });
    }
    // AE View 1 (native index 0) previews the glow input; View 2 renders the final composite.
    if ctx.params.e("PEDG-0023") == 0 {
        b.img = source;
        return b;
    }
    let spread = (ctx.params.f("PEDG-0011") / 100.0).clamp(0.0, 1.0) as f32;
    let near = (0.30 - 0.35 * spread).max(0.05);
    let far = 0.30 + 0.44 * spread;
    // A broad, unnormalised halo keeps thin bright strokes visible far from their source.
    let weights = [near * 13.5, (1.0 - near - far) * 13.5, far * 13.5];
    let aspect = ctx.params.f("PEDG-0010").clamp(0.0, 2.0);
    let chroma = if ctx.params.b("PEDG-0013") { ctx.params.f("PEDG-0015").clamp(0.0, 2.0) as f32 } else { 0.0 };
    let downsample = (ctx.params.f("PEDG-0019") / 100.0).clamp(0.1, 1.0);
    let mut halo = Image::new(b.img.width, b.img.height);
    for (step, (sigma, weight)) in [(0.035, weights[0]), (0.14, weights[1]), (0.42, weights[2])].into_iter().enumerate() {
        let (sx, sy) = (radius * sigma * aspect, radius * sigma * (2.0 - aspect));
        let blurred = if step == 2 && downsample < 0.999 {
            let (w, h) = ((source.width as f64 * downsample).round().max(1.0) as u32, (source.height as f64 * downsample).round().max(1.0) as u32);
            let small = resample(&source, w, h);
            resample(&gaussian_blur(&small, sx * downsample, sy * downsample, false), source.width, source.height)
        } else {
            gaussian_blur(&source, sx, sy, false)
        };
        halo.data.par_iter_mut().zip(blurred.data.par_iter()).for_each(|(out, px)| {
            for c in 0..4 {
                let fringe = if c == 0 {
                    [1.0, 1.0, 1.0 + chroma][step]
                } else if c == 2 {
                    [1.0 + chroma, 1.0, 1.0][step]
                } else {
                    1.0
                };
                out[c] += px[c] * weight * fringe;
            }
        });
    }
    let exposure = ctx.params.f("PEDG-0002").clamp(0.0, 64.0) as f32;
    // Deep Glow's gamma correction applies exposure in approximate linear light.
    let exposure = if ctx.params.b("PEDG-0043") || ctx.params.b("PEDG-0009") { exposure.powf(2.1) } else { exposure };
    let source_opacity = (ctx.params.f("PEDG-0024") / 100.0).clamp(0.0, 1.0) as f32;
    let screen = ctx.params.e("PEDG-0008") == 0;
    let tint = ctx.params.b("PEDG-0037");
    let tint_color = ctx.params.color("PEDG-0038");
    let tint_mode = ctx.params.e("PEDG-0039");
    let tint_mix = (ctx.params.f("PEDG-0041") / 100.0).clamp(0.0, 1.0) as f32;
    b.img.data.par_iter_mut().zip(halo.data.par_iter()).for_each(|(out, glow)| {
        let original = *out;
        for c in 0..3 {
            let mut g = glow[c] * exposure;
            if tint {
                let t = tint_color[c];
                let tinted = match tint_mode {
                    1 if g <= 0.5 => 2.0 * g * t,
                    1 => 1.0 - 2.0 * (1.0 - g) * (1.0 - t),
                    2 if t <= 0.5 => g - (1.0 - 2.0 * t) * g * (1.0 - g),
                    2 => g + (2.0 * t - 1.0) * ((if g <= 0.25 { ((16.0 * g - 12.0) * g + 4.0) * g } else { g.sqrt() }) - g),
                    _ => g * t,
                };
                g += (tinted - g) * tint_mix;
            }
            let s = original[c] * source_opacity;
            out[c] = if screen { (s + g - s * g).clamp(0.0, 1.0) } else { s + g };
        }
        out[3] = (glow[3] * exposure + original[3] * source_opacity).clamp(0.0, 1.0);
    });
    if !ctx.params.b("PEDG-0025") {
        b.img.data.par_iter_mut().for_each(|px| px[3] = 1.0);
    }
    if ctx.params.b("PEDG-0031") {
        let amount = (ctx.params.f("PEDG-0033") / 100.0).clamp(0.0, 1.0) as f32 / 255.0;
        let monochrome = ctx.params.b("PEDG-0032");
        let width = b.img.width;
        b.img.rows_mut().for_each(|(y, row)| {
            for (x, px) in row.iter_mut().enumerate() {
                if px[3] < 0.999 {
                    continue;
                }
                let channel_noise =
                    |c: u32| (hash_noise((x as u32).wrapping_add(if monochrome { 0 } else { c.wrapping_mul(width) }), y as u32, ctx.seed) - 0.5) * amount;
                for c in 0..3 {
                    px[c] = (px[c] + channel_noise(c as u32)).max(0.0);
                }
            }
        });
    }
    b
}

fn drop_shadow(ctx: &EffectCtx, mut b: Buf) -> Buf {
    let color = ctx.params.color("color");
    let opacity = ctx.params.f("opacity") as f32 / 100.0;
    let dir = ctx.params.f("direction").to_radians();
    let dist = ctx.params.f("distance") * b.scale;
    let soft = ctx.params.f("softness") * b.scale;
    let only = ctx.params.b("shadowOnly");
    let pad = (dist + soft * 1.5).ceil() as u32 + 2;
    if !ctx.adjustment {
        b.pad(pad);
    }
    let (dx, dy) = (dir.sin() * dist, -dir.cos() * dist);
    let mut sh = Image::new(b.img.width, b.img.height);
    let src = &b.img;
    sh.rows_mut().for_each(|(y, row)| {
        for (x, px) in row.iter_mut().enumerate() {
            let a = src.sample_bilinear(x as f64 + 0.5 - dx, y as f64 + 0.5 - dy)[3] * opacity * color[3];
            *px = [color[0] * a, color[1] * a, color[2] * a, a];
        }
    });
    if soft > 0.0 {
        sh = gaussian_blur(&sh, soft / 2.0, soft / 2.0, false);
    }
    if !only {
        sh.data.par_iter_mut().zip(b.img.data.par_iter()).for_each(|(s, o)| {
            let k = 1.0 - o[3];
            for c in 0..4 {
                s[c] = o[c] + s[c] * k;
            }
        });
    }
    b.img = sh;
    b
}

/// Invert's Channel options (After Effects order): RGB / HLS / YIQ groups, each followed by its
/// single channels, then Alpha.
pub const INVERT_CHANNELS: [&str; 13] =
    ["RGB", "Red", "Green", "Blue", "HLS", "Hue", "Lightness", "Saturation", "YIQ", "Luminance", "In Phase Chrominance", "Quadrature Chrominance", "Alpha"];
/// Index of "Alpha" in [`INVERT_CHANNELS`].
pub const INVERT_ALPHA: u32 = 12;

fn rgb_to_yiq(c: [f32; 3]) -> [f32; 3] {
    [0.299 * c[0] + 0.587 * c[1] + 0.114 * c[2], 0.596 * c[0] - 0.274 * c[1] - 0.322 * c[2], 0.211 * c[0] - 0.523 * c[1] + 0.312 * c[2]]
}

fn yiq_to_rgb(c: [f32; 3]) -> [f32; 3] {
    [c[0] + 0.956 * c[1] + 0.621 * c[2], c[0] - 0.272 * c[1] - 0.647 * c[2], c[0] - 1.106 * c[1] + 1.703 * c[2]]
}

/// Invert straight colour `s` in the colour space / channel `ch` (an [`INVERT_CHANNELS`] index
/// other than Alpha).
fn invert_color(ch: u32, s: [f32; 3]) -> [f32; 3] {
    match ch {
        1 => [1.0 - s[0], s[1], s[2]],
        2 => [s[0], 1.0 - s[1], s[2]],
        3 => [s[0], s[1], 1.0 - s[2]],
        4..=7 => {
            let (mut h, mut sat, mut l) = effectcraft_color::rgb_to_hsl(s[0], s[1], s[2]);
            if matches!(ch, 4 | 5) {
                h = 1.0 - h;
            }
            if matches!(ch, 4 | 6) {
                l = 1.0 - l;
            }
            if matches!(ch, 4 | 7) {
                sat = 1.0 - sat;
            }
            let (r, g, b) = effectcraft_color::hsl_to_rgb(h, sat, l);
            [r, g, b]
        }
        8..=11 => {
            // Y inverts around 1, the (signed) chrominance axes around 0.
            let mut q = rgb_to_yiq(s);
            if matches!(ch, 8 | 9) {
                q[0] = 1.0 - q[0];
            }
            if matches!(ch, 8 | 10) {
                q[1] = -q[1];
            }
            if matches!(ch, 8 | 11) {
                q[2] = -q[2];
            }
            yiq_to_rgb(q).map(|v| v.max(0.0))
        }
        _ => [1.0 - s[0], 1.0 - s[1], 1.0 - s[2]],
    }
}

fn invert(ctx: &EffectCtx, mut b: Buf) -> Buf {
    let ch = ctx.params.e("channel");
    let blend = 1.0 - ctx.params.f("blend") as f32 / 100.0;
    b.img.data.par_iter_mut().for_each(|px| {
        let a = px[3];
        if ch == INVERT_ALPHA {
            let na = 1.0 - a;
            let k = if a > 0.0 { na / a } else { 0.0 };
            let inv = [px[0] * k, px[1] * k, px[2] * k, na];
            for c in 0..4 {
                px[c] += (inv[c] - px[c]) * blend;
            }
            return;
        }
        if a <= 0.0 {
            return;
        }
        let s = [px[0] / a, px[1] / a, px[2] / a];
        let o = invert_color(ch, s);
        for c in 0..3 {
            px[c] = (s[c] + (o[c] - s[c]) * blend) * a;
        }
    });
    b
}

fn mosaic(ctx: &EffectCtx, mut b: Buf) -> Buf {
    let hb = ctx.params.f("horizontal").round().max(1.0) as usize;
    let vb = ctx.params.f("vertical").round().max(1.0) as usize;
    let (w, h) = (b.img.width as usize, b.img.height as usize);
    let cw = (w as f64 / hb as f64).max(1.0);
    let ch = (h as f64 / vb as f64).max(1.0);
    let src = b.img.clone();
    if ctx.params.b("sharpColors") {
        // Each tile takes the colour of the pixel at its centre.
        b.img.rows_mut().for_each(|(y, row)| {
            let cy = ((y as f64 / ch).floor() + 0.5) * ch;
            for (x, px) in row.iter_mut().enumerate() {
                let cx = ((x as f64 / cw).floor() + 0.5) * cw;
                *px = src.get_clamped(cx as i64, cy as i64);
            }
        });
        return b;
    }
    // Each tile takes the average colour of its region.
    let (nx, ny) = ((w as f64 / cw).ceil() as usize, (h as f64 / ch).ceil() as usize);
    let mut sums = vec![[0.0f64; 5]; nx * ny];
    for y in 0..h {
        let ty = ((y as f64 / ch) as usize).min(ny - 1);
        for x in 0..w {
            let tx = ((x as f64 / cw) as usize).min(nx - 1);
            let p = src.get(x as i64, y as i64);
            let s = &mut sums[ty * nx + tx];
            for c in 0..4 {
                s[c] += p[c] as f64;
            }
            s[4] += 1.0;
        }
    }
    b.img.rows_mut().for_each(|(y, row)| {
        let ty = ((y as f64 / ch) as usize).min(ny - 1);
        for (x, px) in row.iter_mut().enumerate() {
            let tx = ((x as f64 / cw) as usize).min(nx - 1);
            let s = sums[ty * nx + tx];
            *px = [0, 1, 2, 3].map(|c| (s[c] / s[4].max(1.0)) as f32);
        }
    });
    b
}

fn posterize(ctx: &EffectCtx, mut b: Buf) -> Buf {
    let lv = ctx.params.f("level").round().clamp(2.0, 255.0) as f32 - 1.0;
    b.img.map_straight(|c| c.map(|v| (v.clamp(0.0, 1.0) * lv).round() / lv));
    b
}

fn threshold(ctx: &EffectCtx, mut b: Buf) -> Buf {
    let t = ctx.params.f("level") as f32 / 255.0;
    b.img.map_straight(|c| {
        let v = if luminance(c[0], c[1], c[2]) >= t { 1.0 } else { 0.0 };
        [v, v, v]
    });
    b
}

fn find_edges(ctx: &EffectCtx, mut b: Buf) -> Buf {
    let inv = ctx.params.b("invert");
    let blend = ctx.params.f("blend") as f32 / 100.0;
    let src = b.img.clone();
    b.img.rows_mut().for_each(|(y, row)| {
        for (x, px) in row.iter_mut().enumerate() {
            let l = |dx: i64, dy: i64| {
                let p = src.get_clamped(x as i64 + dx, y as i64 + dy);
                luminance(p[0], p[1], p[2])
            };
            let gx = l(1, -1) + 2.0 * l(1, 0) + l(1, 1) - l(-1, -1) - 2.0 * l(-1, 0) - l(-1, 1);
            let gy = l(-1, 1) + 2.0 * l(0, 1) + l(1, 1) - l(-1, -1) - 2.0 * l(0, -1) - l(1, -1);
            let e = (gx * gx + gy * gy).sqrt().min(1.0);
            let v = if inv { e } else { 1.0 - e };
            let a = px[3];
            *px = blend_with(*px, [v * a, v * a, v * a, a], blend);
        }
    });
    b
}

/// Blend With Original: `orig` over the effect result `fx` by `k` (0 = effect only).
fn blend_with(orig: [f32; 4], fx: [f32; 4], k: f32) -> [f32; 4] {
    [0, 1, 2, 3].map(|c| fx[c] + (orig[c] - fx[c]) * k)
}

fn emboss(ctx: &EffectCtx, mut b: Buf) -> Buf {
    let ang = ctx.params.f("direction").to_radians();
    let relief = ctx.params.f("relief") * b.scale;
    let contrast = ctx.params.f("contrast") as f32 / 100.0;
    let blend = ctx.params.f("blend") as f32 / 100.0;
    let (dx, dy) = (ang.cos() * relief, -ang.sin() * relief);
    let src = b.img.clone();
    b.img.rows_mut().for_each(|(y, row)| {
        for (x, px) in row.iter_mut().enumerate() {
            let a = src.sample_bilinear_clamped(x as f64 + 0.5 + dx, y as f64 + 0.5 + dy);
            let c = src.sample_bilinear_clamped(x as f64 + 0.5 - dx, y as f64 + 0.5 - dy);
            let d = luminance(a[0], a[1], a[2]) - luminance(c[0], c[1], c[2]);
            let v = (0.5 + d * contrast).clamp(0.0, 1.0);
            let al = px[3];
            *px = blend_with(*px, [v * al, v * al, v * al, al], blend);
        }
    });
    b
}

fn noise(ctx: &EffectCtx, mut b: Buf) -> Buf {
    let amt = ctx.params.f("amount") as f32 / 100.0;
    let color = ctx.params.b("color");
    // Old saves lack the switch: clipping was always on.
    let clip = ctx.params.get("clip").is_none_or(|v| v.as_bool());
    let frame_seed = (ctx.time * 1000.0) as u32 ^ ctx.seed;
    let w = b.img.width;
    b.img.rows_mut().for_each(|(y, row)| {
        for (x, px) in row.iter_mut().enumerate() {
            let a = px[3];
            if a <= 0.0 {
                continue;
            }
            let n = |k: u32| (hash_noise(x as u32 + k * w, y as u32, frame_seed) - 0.5) * amt;
            let (r, g, bl) = if color {
                (n(0), n(1), n(2))
            } else {
                let v = n(0);
                (v, v, v)
            };
            if clip {
                px[0] = (px[0] + r * a).clamp(0.0, a);
                px[1] = (px[1] + g * a).clamp(0.0, a);
                px[2] = (px[2] + bl * a).clamp(0.0, a);
            } else {
                // Unclipped: values past black / white wrap around (integer overflow look).
                for (c, n) in [r, g, bl].into_iter().enumerate() {
                    px[c] = ((px[c] / a + n).rem_euclid(1.0)) * a;
                }
            }
        }
    });
    b
}

fn linear_wipe(ctx: &EffectCtx, mut b: Buf) -> Buf {
    let done = ctx.params.f("completion") / 100.0;
    let ang = ctx.params.f("angle").to_radians();
    let feather = (ctx.params.f("feather") * b.scale).max(0.001);
    let (w, h) = (b.img.width as f64, b.img.height as f64);
    // Direction the wipe travels: angle 90° wipes left→right.
    let (dx, dy) = (ang.sin(), -ang.cos());
    let corners = [(0.0, 0.0), (w, 0.0), (0.0, h), (w, h)];
    let proj: Vec<f64> = corners.iter().map(|(x, y)| x * dx + y * dy).collect();
    let (lo, hi) = (proj.iter().cloned().fold(f64::INFINITY, f64::min), proj.iter().cloned().fold(f64::NEG_INFINITY, f64::max));
    let edge = lo - feather + (hi - lo + 2.0 * feather) * done;
    b.img.rows_mut().for_each(|(y, row)| {
        for (x, px) in row.iter_mut().enumerate() {
            let d = (x as f64 + 0.5) * dx + (y as f64 + 0.5) * dy;
            let k = ((d - edge) / feather + 0.5).clamp(0.0, 1.0) as f32;
            for c in px.iter_mut() {
                *c *= k;
            }
        }
    });
    b
}

fn radial_wipe(ctx: &EffectCtx, mut b: Buf) -> Buf {
    let done = ctx.params.f("completion") / 100.0;
    let start = ctx.params.f("startAngle");
    let c = b.to_px(ctx.params.v2("center"));
    let dir = ctx.params.e("wipe");
    let feather = (ctx.params.f("feather")).max(0.01);
    b.img.rows_mut().for_each(|(y, row)| {
        for (x, px) in row.iter_mut().enumerate() {
            let a = ((x as f64 + 0.5 - c.0).atan2(-(y as f64 + 0.5 - c.1)).to_degrees() - start).rem_euclid(360.0);
            // Both: the wipe opens in both directions from the start angle at once.
            let (a, edge) = match dir {
                1 => (360.0 - a, done * 360.0),
                2 => (a.min(360.0 - a), done * 180.0),
                _ => (a, done * 360.0),
            };
            let k = ((a - edge) / feather + 0.5).clamp(0.0, 1.0) as f32;
            for ch in px.iter_mut() {
                *ch *= k;
            }
        }
    });
    b
}

fn venetian(ctx: &EffectCtx, mut b: Buf) -> Buf {
    let done = ctx.params.f("completion") / 100.0;
    let ang = ctx.params.f("direction").to_radians();
    let width = (ctx.params.f("width") * b.scale).max(1.0);
    let feather = (ctx.params.f("feather") * b.scale).max(0.001);
    let (dx, dy) = (ang.cos(), ang.sin());
    b.img.rows_mut().for_each(|(y, row)| {
        for (x, px) in row.iter_mut().enumerate() {
            let d = ((x as f64 + 0.5) * dx + (y as f64 + 0.5) * dy).rem_euclid(width);
            let k = ((d - done * width) / feather + 0.5).clamp(0.0, 1.0) as f32;
            for c in px.iter_mut() {
                *c *= k;
            }
        }
    });
    b
}

pub fn specs() -> Vec<EffectSpec> {
    let dims = || popup(&["Horizontal and Vertical", "Horizontal", "Vertical"]);
    vec![
        spec(
            "ec.blur.gaussian",
            "Gaussian Blur",
            "Blur & Sharpen",
            vec![
                p("blurriness", "Blurriness", num(0.0), slider(0.0, 3000.0, 0.0, 50.0, 1)),
                p("dimensions", "Blur Dimensions", Value::Enum(0), dims()),
                p("repeatEdge", "Repeat Edge Pixels", Value::Bool(false), ParamUi::Checkbox),
            ],
            gaussian,
        ),
        spec(
            "ec.blur.fastbox",
            "Fast Box Blur",
            "Blur & Sharpen",
            vec![
                p("radius", "Blur Radius", num(0.0), slider(0.0, 3000.0, 0.0, 50.0, 1)),
                p("iterations", "Iterations", num(3.0), slider(1.0, 50.0, 1.0, 5.0, 0)),
                p("dimensions", "Blur Dimensions", Value::Enum(0), dims()),
                p("repeatEdge", "Repeat Edge Pixels", Value::Bool(false), ParamUi::Checkbox),
            ],
            box_blur,
        ),
        spec(
            "ec.blur.directional",
            "Directional Blur",
            "Blur & Sharpen",
            vec![p("direction", "Direction", num(0.0), ParamUi::Angle), p("length", "Blur Length", num(0.0), slider(0.0, 1000.0, 0.0, 100.0, 1))],
            directional,
        ),
        spec(
            "ec.blur.radial",
            "Radial Blur",
            "Blur & Sharpen",
            vec![
                p("amount", "Amount", num(10.0), slider(0.0, 100.0, 0.0, 100.0, 1)),
                p("center", "Center", Value::Vec2([0.5, 0.5]), ParamUi::Point),
                p("type", "Type", Value::Enum(0), popup(&["Spin", "Zoom"])),
                // Our blur is always fully anti-aliased; the choice is kept for compatibility.
                p("antialiasing", "Antialiasing (Best Quality)", Value::Enum(0), popup(&["Low", "High"])),
            ],
            radial,
        ),
        spec("ec.blur.sharpen", "Sharpen", "Blur & Sharpen", vec![p("amount", "Sharpen Amount", num(0.0), slider(0.0, 500.0, 0.0, 100.0, 0))], sharpen),
        spec(
            "ec.blur.unsharp",
            "Unsharp Mask",
            "Blur & Sharpen",
            vec![
                p("amount", "Amount", num(50.0), slider(0.0, 500.0, 0.0, 500.0, 0)),
                p("radius", "Radius", num(1.0), slider(0.1, 500.0, 0.1, 50.0, 1)),
                p("threshold", "Threshold", num(0.0), slider(0.0, 255.0, 0.0, 255.0, 0)),
            ],
            unsharp,
        ),
        spec(
            "ec.stylize.glow",
            "Glow",
            "Stylize",
            vec![
                p("based", "Glow Based On", Value::Enum(1), popup(&["Alpha Channel", "Color Channels"])),
                p("threshold", "Glow Threshold", num(60.0), slider(0.0, 100.0, 0.0, 100.0, 1)),
                p("radius", "Glow Radius", num(10.0), slider(0.0, 1000.0, 0.0, 100.0, 1)),
                p("intensity", "Glow Intensity", num(1.0), slider(0.0, 255.0, 0.0, 4.0, 1)),
                p("operation", "Composite Original", Value::Enum(0), popup(&["On Top", "Behind", "None"])),
                p("glowOperation", "Glow Operation", Value::Enum(1), popup(&GLOW_OPERATIONS)),
                p("colors", "Glow Colors", Value::Enum(0), popup(&["Original Colors", "A & B Colors", "Arbitrary Map"])),
                // Arbitrary Map: "red points | green points | blue points" (Curves format).
                p("arbitraryMap", "Arbitrary Map", Value::Str(String::new()), ParamUi::Hidden),
                p("colorLooping", "Color Looping", Value::Enum(2), popup(&["Sawtooth A>B", "Sawtooth B>A", "Triangle A>B>A", "Triangle B>A>B"])),
                p("colorLoops", "Color Loops", num(1.0), slider(1.0, 127.0, 1.0, 10.0, 1)),
                p("colorPhase", "Color Phase", num(0.0), ParamUi::Angle),
                p("abMidpoint", "A & B Midpoint", num(50.0), slider(0.0, 100.0, 0.0, 100.0, 1)),
                p("colorA", "Color A", col(1.0, 1.0, 1.0), ParamUi::Color),
                p("colorB", "Color B", col(0.0, 0.0, 0.0), ParamUi::Color),
                p("glowDimensions", "Glow Dimensions", Value::Enum(0), dims()),
            ],
            glow,
        ),
        spec(
            "ec.stylize.deepglow",
            "Deep Glow (Native)",
            "Stylize",
            vec![
                p("PEDG-0001", "Radius", num(116.0), slider(0.0, 2000.0, 0.0, 500.0, 0)),
                p("PEDG-0002", "Exposure", num(1.0), slider(0.0, 64.0, 0.0, 4.0, 2)),
                p("PEDG-0004", "Threshold", num(0.0), slider(0.0, 1600.0, 0.0, 100.0, 0)),
                p("PEDG-0005", "Threshold Smooth", num(0.0), slider(0.0, 100.0, 0.0, 100.0, 0)),
                p("PEDG-0008", "Blend Mode", Value::Enum(0), popup(&["Screen", "Add"])),
                p("PEDG-0009", "Gamma Correction", Value::Bool(false), ParamUi::Checkbox),
                p("PEDG-0010", "Aspect Ratio", num(1.0), slider(0.0, 2.0, 0.0, 2.0, 2)),
                p("PEDG-0011", "Spread", num(33.0), slider(0.0, 100.0, 0.0, 100.0, 0)),
                p("PEDG-0019", "Downsample", num(80.0), slider(10.0, 100.0, 10.0, 100.0, 0)),
                p("PEDG-0025", "Unmult (Generate Alpha)", Value::Bool(true), ParamUi::Checkbox),
                p("PEDG-0031", "Dither", Value::Bool(true), ParamUi::Checkbox),
                p("PEDG-0032", "Monochromatic Dither", Value::Bool(false), ParamUi::Checkbox),
                p("PEDG-0033", "Dither Amount", num(50.0), slider(0.0, 100.0, 0.0, 100.0, 0)),
                p("PEDG-0055", "Edge Behaviour", Value::Enum(1), slider(0.0, 2.0, 0.0, 2.0, 0)),
                p("PEDG-0084", "Buffer Expansion", Value::Enum(0), popup(&["Auto", "Compatibility"])),
                p("PEDG-0013", "Chromatic Aberration", Value::Bool(false), ParamUi::Checkbox),
                p("PEDG-0015", "Chromatic Amount", num(0.25), slider(0.0, 2.0, 0.0, 2.0, 2)),
                p("PEDG-0037", "Tint", Value::Bool(false), ParamUi::Checkbox),
                p("PEDG-0038", "Tint Color", col(1.0, 0.0, 0.0), ParamUi::Color),
                p("PEDG-0039", "Tint Mode", Value::Enum(0), popup(&["Standard", "Overlay", "Soft Light"])),
                p("PEDG-0041", "Tint Mix", num(100.0), slider(0.0, 100.0, 0.0, 100.0, 0)),
                p("PEDG-0043", "Auto Detect Gamma", Value::Bool(true), ParamUi::Checkbox),
                p("PEDG-0056", "Mask Layer", Value::Layer(None), ParamUi::Layer),
                p("PEDG-0057", "Mask Mode", Value::Enum(0), popup(&["Alpha", "Luminance"])),
                p("PEDG-0059", "Invert Mask", Value::Bool(false), ParamUi::Checkbox),
                p("PEDG-0023", "View", Value::Enum(1), popup(&["Glow Input", "Final"])),
                p("PEDG-0024", "Source Opacity", num(100.0), slider(0.0, 100.0, 0.0, 100.0, 0)),
            ],
            deep_glow,
        ),
        spec(
            "ec.stylize.mosaic",
            "Mosaic",
            "Stylize",
            vec![
                p("horizontal", "Horizontal Blocks", num(10.0), slider(1.0, 4000.0, 1.0, 200.0, 0)),
                p("vertical", "Vertical Blocks", num(10.0), slider(1.0, 4000.0, 1.0, 200.0, 0)),
                p("sharpColors", "Sharp Colors", Value::Bool(false), ParamUi::Checkbox),
            ],
            mosaic,
        ),
        spec("ec.stylize.posterize", "Posterize", "Stylize", vec![p("level", "Level", num(6.0), slider(2.0, 255.0, 2.0, 32.0, 0))], posterize),
        spec("ec.stylize.threshold", "Threshold", "Stylize", vec![p("level", "Level", num(128.0), slider(0.0, 255.0, 0.0, 255.0, 0))], threshold),
        spec(
            "ec.stylize.findedges",
            "Find Edges",
            "Stylize",
            vec![p("invert", "Invert", Value::Bool(false), ParamUi::Checkbox), p("blend", "Blend With Original", num(0.0), slider(0.0, 100.0, 0.0, 100.0, 0))],
            find_edges,
        ),
        spec(
            "ec.stylize.emboss",
            "Emboss",
            "Stylize",
            vec![
                p("direction", "Direction", num(45.0), ParamUi::Angle),
                p("relief", "Relief", num(1.5), slider(0.0, 10.0, 0.0, 10.0, 2)),
                p("contrast", "Contrast", num(100.0), slider(0.0, 500.0, 0.0, 500.0, 0)),
                p("blend", "Blend With Original", num(0.0), slider(0.0, 100.0, 0.0, 100.0, 0)),
            ],
            emboss,
        ),
        spec(
            "ec.perspective.dropshadow",
            "Drop Shadow",
            "Perspective",
            vec![
                p("color", "Shadow Color", col(0.0, 0.0, 0.0), ParamUi::Color),
                p("opacity", "Opacity", num(50.0), slider(0.0, 100.0, 0.0, 100.0, 0)),
                p("direction", "Direction", num(135.0), ParamUi::Angle),
                p("distance", "Distance", num(5.0), slider(0.0, 32000.0, 0.0, 120.0, 1)),
                p("softness", "Softness", num(0.0), slider(0.0, 1000.0, 0.0, 250.0, 1)),
                p("shadowOnly", "Shadow Only", Value::Bool(false), ParamUi::Checkbox),
            ],
            drop_shadow,
        ),
        spec(
            "ec.channel.invert",
            "Invert",
            "Channel",
            vec![
                p("channel", "Channel", Value::Enum(0), popup(&INVERT_CHANNELS)),
                p("blend", "Blend With Original", num(0.0), slider(0.0, 100.0, 0.0, 100.0, 0)),
            ],
            invert,
        ),
        spec(
            "ec.noise.noise",
            "Noise",
            "Noise & Grain",
            vec![
                p("amount", "Amount of Noise", num(0.0), slider(0.0, 100.0, 0.0, 100.0, 1)),
                p("color", "Use Color Noise", Value::Bool(true), ParamUi::Checkbox),
                p("clip", "Clip Result Values", Value::Bool(true), ParamUi::Checkbox),
            ],
            noise,
        ),
        spec(
            "ec.transition.linearwipe",
            "Linear Wipe",
            "Transition",
            vec![
                p("completion", "Transition Completion", num(0.0), slider(0.0, 100.0, 0.0, 100.0, 0)),
                p("angle", "Wipe Angle", num(90.0), ParamUi::Angle),
                p("feather", "Feather", num(0.0), slider(0.0, 32000.0, 0.0, 500.0, 1)),
            ],
            linear_wipe,
        ),
        spec(
            "ec.transition.radialwipe",
            "Radial Wipe",
            "Transition",
            vec![
                p("completion", "Transition Completion", num(0.0), slider(0.0, 100.0, 0.0, 100.0, 0)),
                p("startAngle", "Start Angle", num(0.0), ParamUi::Angle),
                p("center", "Wipe Center", Value::Vec2([0.5, 0.5]), ParamUi::Point),
                p("wipe", "Wipe", Value::Enum(0), popup(&["Clockwise", "Counterclockwise", "Both"])),
                p("feather", "Feather", num(0.0), slider(0.0, 360.0, 0.0, 50.0, 1)),
            ],
            radial_wipe,
        ),
        spec(
            "ec.transition.venetian",
            "Venetian Blinds",
            "Transition",
            vec![
                p("completion", "Transition Completion", num(0.0), slider(0.0, 100.0, 0.0, 100.0, 0)),
                p("direction", "Direction", num(90.0), ParamUi::Angle),
                p("width", "Width", num(30.0), slider(2.0, 1000.0, 2.0, 200.0, 0)),
                p("feather", "Feather", num(0.0), slider(0.0, 100.0, 0.0, 100.0, 1)),
            ],
            venetian,
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EffectEnv, run_fx};

    /// A bright square on mid grey.
    fn scene() -> effectcraft_raster::Image {
        let mut img = effectcraft_raster::Image::filled(24, 24, [0.3, 0.3, 0.3, 1.0]);
        for y in 8..16 {
            for x in 8..16 {
                img.set(x, y, [1.0, 1.0, 1.0, 1.0]);
            }
        }
        img
    }

    #[test]
    fn glow_operation_and_arbitrary_map() {
        // Pixels addressed in layer space (Glow pads its buffer).
        struct Out(Buf);
        impl Out {
            fn get(&self, x: i64, y: i64) -> [f32; 4] {
                self.0.img.get(x + self.0.offset[0] as i64, y + self.0.offset[1] as i64)
            }
        }
        let run = |vals: &[(&str, Value)]| Out(run_fx("ec.stylize.glow", vals, scene(), 0.0, EffectEnv::default()));
        let add = run(&[]);
        assert_eq!(GLOW_OPERATIONS[1], "Add");
        // Multiply darkens around the square where Add brightens.
        let mul = run(&[("glowOperation", Value::Enum(2))]);
        assert!(add.get(6, 12)[0] > 0.31 && mul.get(6, 12)[0] <= 0.3 + 1e-4, "{:?} {:?}", add.get(6, 12), mul.get(6, 12));
        assert_eq!(
            glow_operation(&EffectCtx {
                params: &crate::Params::default(),
                time: 0.0,
                layer_size: [1.0; 2],
                seed: 0,
                adjustment: false,
                env: Default::default()
            }),
            effectcraft_color::BlendMode::Add
        );
        // Arbitrary Map: the glow takes the mapped colour (here pure red for every brightness).
        let red = run(&[("colors", Value::Enum(2)), ("arbitraryMap", Value::Str("0,1 1,1|0,0 1,0|0,0 1,0".into())), ("operation", Value::Enum(2))]);
        let p = red.get(6, 12);
        assert!(p[0] > 0.05 && p[1] < 1e-4 && p[2] < 1e-4, "{p:?}");
    }

    #[test]
    fn deep_glow_renders_editable_halo() {
        assert_eq!(crate::lookup("PEDG").map(|s| s.id), Some("ec.stylize.deepglow"));
        let mut img = Image::new(17, 17);
        img.set(8, 8, [1.0, 0.25, 0.1, 1.0]);
        let run = |vals: &[(&str, Value)]| run_fx("ec.stylize.deepglow", vals, img.clone(), 0.0, EffectEnv::default());
        let base = run(&[("PEDG-0001", num(8.0))]);
        let pixel = |b: &Buf, x: i64, y: i64| b.img.get(x + b.offset[0] as i64, y + b.offset[1] as i64);
        assert!(pixel(&base, 11, 8)[0] > 0.0, "radius must create a halo");
        assert!(pixel(&base, 11, 8)[0] > pixel(&base, 11, 8)[1], "halo keeps source hue");
        let brighter = run(&[("PEDG-0001", num(8.0)), ("PEDG-0002", num(2.0))]);
        assert!(pixel(&brighter, 11, 8)[0] > pixel(&base, 11, 8)[0], "exposure must brighten halo");
        let cut = run(&[("PEDG-0001", num(8.0)), ("PEDG-0004", num(200.0))]);
        assert_eq!(pixel(&cut, 11, 8)[0], 0.0, "threshold must remove dim sources");
        let source = run(&[("PEDG-0001", num(8.0)), ("PEDG-0023", Value::Enum(0))]);
        assert_eq!(pixel(&source, 11, 8)[0], 0.0, "input view must omit halo");
        let lower_quality = run(&[("PEDG-0001", num(8.0)), ("PEDG-0019", num(75.0))]);
        assert_ne!(pixel(&lower_quality, 11, 8), pixel(&base, 11, 8), "Downsample must change the broad halo");
        let clipped = run(&[("PEDG-0001", num(8.0)), ("PEDG-0055", Value::Enum(0))]);
        assert_eq!(clipped.offset, [0.0, 0.0], "edge mode 0 keeps layer bounds");
        let compatible = run(&[("PEDG-0001", num(8.0)), ("PEDG-0084", Value::Enum(1))]);
        assert!(compatible.offset[0] > base.offset[0], "compatibility buffer expands farther");
        // The AEP importer converts one-based AE enum values to zero-based native indices.
        let native = |raw: u32| Value::Enum(raw - 1);
        let corpus = run(&[("PEDG-0001", num(8.0)), ("PEDG-0008", native(1)), ("PEDG-0023", native(2)), ("PEDG-0055", native(2)), ("PEDG-0084", native(1))]);
        assert_eq!(corpus.offset, base.offset);
        assert!(pixel(&corpus, 11, 8)[0] > 0.0, "AE View 2 must render final glow");
        let add = run(&[("PEDG-0001", num(8.0)), ("PEDG-0008", native(2))]);
        assert!(pixel(&add, 8, 8)[0] > pixel(&corpus, 8, 8)[0], "AE Blend Mode 2 must use unclamped Add");
        let low_auto = run(&[("PEDG-0001", num(8.0)), ("PEDG-0002", num(0.37))]);
        let low_linear = run(&[("PEDG-0001", num(8.0)), ("PEDG-0002", num(0.37)), ("PEDG-0043", Value::Bool(false))]);
        assert!(pixel(&low_auto, 11, 8)[0] < pixel(&low_linear, 11, 8)[0], "auto gamma must temper subunit exposure");
        let low_manual = run(&[("PEDG-0001", num(8.0)), ("PEDG-0002", num(0.37)), ("PEDG-0043", Value::Bool(false)), ("PEDG-0009", Value::Bool(true))]);
        assert_eq!(pixel(&low_auto, 11, 8), pixel(&low_manual, 11, 8), "manual gamma can replace auto detection");
        let unit_linear = run(&[("PEDG-0001", num(8.0)), ("PEDG-0043", Value::Bool(false))]);
        assert_eq!(pixel(&base, 11, 8), pixel(&unit_linear, 11, 8), "unit exposure stays calibrated");
        let tint = [("PEDG-0001", num(8.0)), ("PEDG-0037", Value::Bool(true)), ("PEDG-0038", Value::Color([0.8, 0.2, 0.5, 1.0]))];
        let standard = run(&[tint[0].clone(), tint[1].clone(), tint[2].clone(), ("PEDG-0039", native(1))]);
        let overlay = run(&[tint[0].clone(), tint[1].clone(), tint[2].clone(), ("PEDG-0039", native(2))]);
        assert_ne!(pixel(&standard, 11, 8), pixel(&overlay, 11, 8), "Tint Mode must alter the glow");
        let host = crate::channel2::tests::FakeHost(Image::new(17, 17));
        let masked = |invert| {
            run_fx(
                "ec.stylize.deepglow",
                &[("PEDG-0001", num(8.0)), ("PEDG-0056", Value::Layer(Some(7))), ("PEDG-0059", Value::Bool(invert))],
                img.clone(),
                0.0,
                EffectEnv { host: Some(&host), ..Default::default() },
            )
        };
        assert_eq!(pixel(&masked(false), 11, 8)[0], 0.0, "transparent mask must suppress the glow");
        assert!(pixel(&masked(true), 11, 8)[0] > 0.0, "inverted mask restores the glow");

        let mut impulse = Image::new(9, 9);
        impulse.set(4, 4, [1.0; 4]);
        let wide = run_fx("ec.stylize.deepglow", &[("PEDG-0001", num(320.0))], impulse.clone(), 0.0, EffectEnv::default());
        assert!(pixel(&wide, 284, 4)[0] > 0.0, "large SceneYard radii must extend beyond 256 px");
        let opaque = run_fx("ec.stylize.deepglow", &[("PEDG-0001", num(8.0)), ("PEDG-0025", Value::Bool(false))], impulse, 0.0, EffectEnv::default());
        assert_eq!(pixel(&opaque, 7, 7)[3], 1.0, "Unmult off keeps a black opaque backdrop");
        let gray = Image::filled(9, 9, [0.3, 0.3, 0.3, 1.0]);
        let dithered = run_fx("ec.stylize.deepglow", &[("PEDG-0001", num(8.0))], gray.clone(), 0.0, EffectEnv::default());
        let clean = run_fx("ec.stylize.deepglow", &[("PEDG-0001", num(8.0)), ("PEDG-0031", Value::Bool(false))], gray, 0.0, EffectEnv::default());
        assert!(dithered.img.data.iter().zip(&clean.img.data).any(|(a, b)| a != b), "Dither must affect opaque pixels");
    }
}
