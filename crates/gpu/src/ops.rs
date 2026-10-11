//! Compositing primitives: each mirrors a CPU operation in `effectcraft-raster` /
//! `effectcraft-render` and returns a new image (GPU images are immutable; only [`warp_q_in`]
//! draws on a canvas in place, one no one else can see).

use effectcraft_color::{BlendMode, Conversion};
use effectcraft_geom::{Mat3, Rect, vec2};
use effectcraft_project::MatteKind;
use effectcraft_raster::Sampling;

use crate::context::{Enc, GpuImage, Params};

/// Index of a blend mode in `BlendMode::ALL` (the kernels' mode numbers).
pub fn mode_id(m: BlendMode) -> u32 {
    BlendMode::ALL.iter().position(|x| *x == m).unwrap_or(0) as u32
}

fn sampling_id(s: Sampling) -> u32 {
    match s {
        Sampling::Nearest => 0,
        Sampling::Bilinear => 1,
        Sampling::Bicubic => 2,
    }
}

fn rows(m: &Mat3) -> [[f32; 4]; 3] {
    m.0.map(|r| [r[0] as f32, r[1] as f32, r[2] as f32, 0.0])
}

/// Box-filter halving (minification pre-filter).
pub fn half(e: &mut Enc, src: &GpuImage) -> GpuImage {
    let out = e.scratch(src.width.div_ceil(2).max(1), src.height.div_ceil(2).max(1));
    e.pixels("half", &Params::default(), src, None, &out, None);
    out
}

/// `raster::composite_warp` (and, with `accumulate = Some(weight)`, `accumulate_warp`): draw
/// `src` transformed by `m` (src pixels → dst pixels) onto `dst`.
pub fn warp(
    e: &mut Enc,
    dst: &GpuImage,
    src: &GpuImage,
    m: &Mat3,
    sampling: Sampling,
    mode: BlendMode,
    opacity: f32,
    seed: u32,
    accumulate: Option<f32>,
) -> GpuImage {
    warp_q(e, dst, src, m, sampling, mode, opacity, seed, accumulate, None)
}

/// [`warp`], clamping and quantising the result to `levels` (8/16 bpc: the comp after a layer,
/// in the same pass).
#[allow(clippy::too_many_arguments)]
pub fn warp_q(
    e: &mut Enc,
    dst: &GpuImage,
    src: &GpuImage,
    m: &Mat3,
    sampling: Sampling,
    mode: BlendMode,
    opacity: f32,
    seed: u32,
    accumulate: Option<f32>,
    levels: Option<f32>,
) -> GpuImage {
    warp_onto(e, dst, src, m, sampling, mode, opacity, seed, accumulate, levels, false).unwrap_or_else(|| dst.clone())
}

/// [`warp_q`] onto `dst` itself (`*dst = warp_q(e, dst, ..)`, the same pixels): drawn in
/// place over the layer's bounds only when this encoder owns `dst` ([`Enc::owns`]), instead of
/// writing a whole new canvas per layer.
#[allow(clippy::too_many_arguments)]
pub(crate) fn warp_q_in(
    e: &mut Enc,
    dst: &mut GpuImage,
    src: &GpuImage,
    m: &Mat3,
    sampling: Sampling,
    mode: BlendMode,
    opacity: f32,
    seed: u32,
    accumulate: Option<f32>,
    levels: Option<f32>,
) {
    let owned = e.owns(dst);
    match warp_onto(e, dst, src, m, sampling, mode, opacity, seed, accumulate, levels, owned) {
        Some(out) => *dst = out,
        None => dst.levels = levels,
    }
}

/// [`warp_q`]'s pass; with `in_place`, drawn into `dst` itself when [`warp_in_place`] can
/// (`None`).
#[allow(clippy::too_many_arguments)]
fn warp_onto(
    e: &mut Enc,
    dst: &GpuImage,
    src: &GpuImage,
    m: &Mat3,
    sampling: Sampling,
    mode: BlendMode,
    opacity: f32,
    seed: u32,
    accumulate: Option<f32>,
    levels: Option<f32>,
    in_place: bool,
) -> Option<GpuImage> {
    if src.width == 0 || src.height == 0 || opacity <= 0.0 {
        return Some(dst.clone());
    }
    let Some(inv0) = m.inverse() else { return Some(dst.clone()) };
    // Minification: pick a pre-filtered level so sampling doesn't alias.
    let scale = m.mean_scale();
    let mut img = src.clone();
    let mut inv = inv0;
    if scale < 0.5 && m.is_affine() {
        let mut s = scale * 2.0;
        let mut f = 0.5;
        img = half(e, src);
        while s < 0.5 && img.width > 1 && img.height > 1 {
            img = half(e, &img);
            f *= 0.5;
            s *= 2.0;
        }
        inv = Mat3::scale(vec2(f, f)) * inv;
    }
    let bounds = m.map_rect(&Rect::from_size(src.width as f64, src.height as f64)).inflate(1.0).round_out();
    let area = bounds.intersect(&Rect::from_size(dst.width as f64, dst.height as f64));
    if area.is_empty() {
        return Some(dst.clone());
    }
    let (x0, x1) = (area.x0.max(0.0) as u32, area.x1.min(dst.width as f64) as u32);
    let (y0, y1) = (area.y0.max(0.0) as u32, area.y1.min(dst.height as f64) as u32);
    let mut p = Params::default();
    let clear = matches!(mode, BlendMode::StencilAlpha | BlendMode::StencilLuma);
    let flags = u32::from(accumulate.is_some()) | if clear && accumulate.is_none() { 2 } else { 0 };
    p.u[0] = [mode_id(mode), sampling_id(sampling), seed, flags];
    let r = rows(&inv);
    p.f[0] = r[0];
    p.f[1] = r[1];
    p.f[2] = r[2];
    p.f[3] = [opacity, accumulate.unwrap_or(1.0), levels.unwrap_or(0.0), levels.map_or(0.0, |v| 1.0 / v)];
    p.f[4] = [x0 as f32, y0 as f32, x1 as f32, y1 as f32];
    if in_place && warp_in_place(e, dst, &img, &p, [x0, y0, x1, y1], levels) {
        return None;
    }
    let mut out = e.scratch(dst.width, dst.height);
    e.pixels("warp_blend", &p, &img, Some(dst), &out, None);
    out.levels = levels;
    Some(out)
}

/// `warp_blend` in place: only the layer's bounds `[x0, y0, x1, y1)` of the canvas `dst` are
/// read and written (`warp_blend_at`), with the same per-pixel arithmetic and quantisation as
/// the full pass, which writes the pixels outside the bounds back unchanged. `false` (nothing
/// recorded) when the full pass must run instead: stencil modes clear outside the bounds, a
/// canvas not already quantised to `levels` is quantised outside them too, and a layer
/// covering half the canvas or more is cheaper as one full pass (in place, its bounds are
/// copied out, then read and written again).
fn warp_in_place(e: &mut Enc, dst: &GpuImage, img: &GpuImage, p: &Params, [x0, y0, x1, y1]: [u32; 4], levels: Option<f32>) -> bool {
    let (w, h) = (x1.saturating_sub(x0), y1.saturating_sub(y0));
    let area = u64::from(w).saturating_mul(u64::from(h));
    let canvas = u64::from(dst.width).saturating_mul(u64::from(dst.height));
    // Quantising is idempotent (see `quantize`): a canvas quantised to `levels` stays as it is.
    let outside_unchanged = p.u[0][3] & 2 == 0 && (levels.is_none() || dst.levels == levels);
    if !outside_unchanged || w == 0 || h == 0 || area.saturating_mul(2) >= canvas || x1 > dst.width || y1 > dst.height || !e.has_kernel("warp_blend_at") {
        return false;
    }
    // The kernel can't read and write one texture in a pass: it reads the bounds from a copy.
    let under = e.crop(dst, x0, y0, w, h);
    let mut q = *p;
    q.u[1] = [x0, y0, x1, y1];
    e.dispatch("warp_blend_at", &q, img, Some(&under), dst, None, (w.div_ceil(16), h.div_ceil(16)));
    true
}

/// Motion-blur sub-samples: `dst` plus `src` warped by each of `matrices` times `weight`, summed
/// in order. The same sums as one [`warp`] accumulate pass per matrix, in far fewer passes: each
/// minification level of `src` is built once and shared by the samples that use it, consecutive
/// samples at the same level sum in one `warp_accum` pass (one write of the canvas instead of one
/// per sample), and samples that land entirely off `dst` (which add exactly nothing) are skipped.
pub fn warp_accumulate(e: &mut Enc, dst: &GpuImage, src: &GpuImage, matrices: &[Mat3], sampling: Sampling, weight: f32) -> GpuImage {
    if src.width == 0 || src.height == 0 || !e.has_kernel("warp_accum") {
        return matrices.iter().fold(dst.clone(), |acc, m| warp(e, &acc, src, m, sampling, BlendMode::Normal, 1.0, 0, Some(weight)));
    }
    // Minification chain shared by the samples (level i = `src` halved i times).
    let mut mips: Vec<GpuImage> = vec![src.clone()];
    // Runs of consecutive samples at the same level: (level, per-sample data, union bounds).
    let mut runs: Vec<(usize, Vec<f32>, [f32; 4])> = Vec::new();
    for m in matrices {
        let Some(inv0) = m.inverse() else { continue };
        // As `warp_q`: a pre-filtered level so sampling doesn't alias.
        let scale = m.mean_scale();
        let (mut level, mut inv) = (0usize, inv0);
        if scale < 0.5 && m.is_affine() {
            let (mut s, mut f) = (scale * 2.0, 0.5);
            level = 1;
            loop {
                while mips.len() <= level {
                    let Some(prev) = mips.last() else { break };
                    let next = half(e, prev);
                    mips.push(next);
                }
                let Some(img) = mips.get(level) else { break };
                if !(s < 0.5 && img.width > 1 && img.height > 1) {
                    break;
                }
                level += 1;
                f *= 0.5;
                s *= 2.0;
            }
            inv = Mat3::scale(vec2(f, f)) * inv;
        }
        let bounds = m.map_rect(&Rect::from_size(src.width as f64, src.height as f64)).inflate(1.0).round_out();
        let area = bounds.intersect(&Rect::from_size(dst.width as f64, dst.height as f64));
        if area.is_empty() {
            continue;
        }
        let b = [
            area.x0.max(0.0) as u32 as f32,
            area.y0.max(0.0) as u32 as f32,
            area.x1.min(dst.width as f64) as u32 as f32,
            area.y1.min(dst.height as f64) as u32 as f32,
        ];
        let r = rows(&inv);
        let sample = [r[0], r[1], r[2], b];
        match runs.last_mut() {
            Some((l, data, u)) if *l == level => {
                data.extend(sample.iter().flatten());
                *u = [u[0].min(b[0]), u[1].min(b[1]), u[2].max(b[2]), u[3].max(b[3])];
            }
            _ => runs.push((level, sample.iter().flatten().copied().collect(), b)),
        }
    }
    let mut acc = dst.clone();
    for (level, data, u) in &runs {
        let Some(img) = mips.get(*level) else { continue };
        let mut p = Params::default();
        p.u[0] = [sampling_id(sampling), (data.len() / 16) as u32, 0, 0];
        p.f[0] = [weight, u[0], u[1], u[2]];
        p.f[1][0] = u[3];
        let buf = e.data(data);
        let out = e.scratch(dst.width, dst.height);
        e.pixels("warp_accum", &p, img, Some(&acc), &out, Some(&buf));
        acc = out;
    }
    acc
}

/// `Image::blend_from`: blend `src` (same size) onto `dst`, clamping and quantising the result
/// to `levels` when set (see [`warp_q`]).
pub fn blend_full(e: &mut Enc, dst: &GpuImage, src: &GpuImage, mode: BlendMode, opacity: f32, seed: u32, levels: Option<f32>) -> GpuImage {
    blend_full_preserve(e, dst, src, mode, opacity, seed, levels, false)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn blend_full_preserve(
    e: &mut Enc,
    dst: &GpuImage,
    src: &GpuImage,
    mode: BlendMode,
    opacity: f32,
    seed: u32,
    levels: Option<f32>,
    preserve: bool,
) -> GpuImage {
    let mut p = Params::default();
    p.u[0] = [mode_id(mode), preserve as u32, seed, 0];
    p.f[0] = [opacity, levels.unwrap_or(0.0), levels.map_or(0.0, |v| 1.0 / v), 0.0];
    let mut out = e.scratch(dst.width, dst.height);
    e.pixels("blend_full", &p, src, Some(dst), &out, None);
    out.levels = levels;
    out
}

/// Multiply an isolated layer by its track matte.
pub fn matte(e: &mut Enc, iso: &GpuImage, matte: &GpuImage, kind: MatteKind) -> GpuImage {
    let mut p = Params::default();
    p.u[0][0] = match kind {
        MatteKind::Alpha => 0,
        MatteKind::AlphaInverted => 1,
        MatteKind::Luma => 2,
        MatteKind::LumaInverted => 3,
    };
    let out = e.scratch(iso.width, iso.height);
    e.pixels("matte", &p, iso, Some(matte), &out, None);
    out
}

/// Knockout: clear what lies below the layer's content.
pub fn knockout(e: &mut Enc, canvas: &GpuImage, shape: &GpuImage) -> GpuImage {
    let out = e.scratch(canvas.width, canvas.height);
    e.pixels("knockout", &Params::default(), canvas, Some(shape), &out, None);
    out
}

/// Styled-layer finish: move `canvas` toward `tmp` by `opacity` on the enabled channels.
pub fn channel_mix(e: &mut Enc, canvas: &GpuImage, tmp: &GpuImage, channels: [bool; 3], opacity: f32) -> GpuImage {
    let mut p = Params::default();
    p.u[0] = [channels[0] as u32, channels[1] as u32, channels[2] as u32, 0];
    p.f[0][0] = opacity;
    let out = e.scratch(canvas.width, canvas.height);
    e.pixels("channel_mix", &p, canvas, Some(tmp), &out, None);
    out
}

/// Adjustment layer finish: move `canvas` toward `adjusted` by `matte`'s alpha × `opacity`.
pub fn adjust_mix(e: &mut Enc, canvas: &GpuImage, adjusted: &GpuImage, matte: &GpuImage, opacity: f32) -> GpuImage {
    let (rows, stride) = e.image_rows(matte);
    let mut p = Params::default();
    p.u[0][0] = stride;
    p.f[0][0] = opacity;
    let out = e.scratch(canvas.width, canvas.height);
    e.pixels("adjust_mix", &p, canvas, Some(adjusted), &out, Some(&rows));
    out
}

/// Clamp and quantise to `levels` steps (8/16 bpc).
pub fn quantize(e: &mut Enc, img: &GpuImage, levels: f32) -> GpuImage {
    // Quantising is idempotent: an image a fused kernel already quantised stays as it is.
    if img.levels == Some(levels) {
        return img.clone();
    }
    let mut p = Params::default();
    p.f[0][0] = levels;
    p.f[0][1] = 1.0 / levels;
    let mut out = e.scratch(img.width, img.height);
    e.pixels("quantize", &p, img, None, &out, None);
    out.levels = Some(levels);
    out
}

fn curve_id(s: Option<effectcraft_color::ColorSpace>) -> u32 {
    use effectcraft_color::space::Curve;
    match s.map(|s| s.curve()) {
        None | Some(Curve::Linear) => 0,
        Some(Curve::Srgb) => 1,
        Some(Curve::Gamma24) => 2,
        Some(Curve::Pq) => 3,
        Some(Curve::Hlg) => 4,
    }
}

/// Colour conversion on straight colour (`render::color::convert`).
pub fn convert(e: &mut Enc, img: &GpuImage, c: &Conversion) -> GpuImage {
    let mut p = Params::default();
    p.u[0] = [curve_id(c.decode), c.matrix.is_some() as u32, curve_id(c.encode), 0];
    if let Some(m) = c.matrix {
        for i in 0..3 {
            p.f[i] = [m[i][0], m[i][1], m[i][2], 0.0];
        }
    }
    let out = e.scratch(img.width, img.height);
    e.pixels("convert", &p, img, None, &out, None);
    out
}
