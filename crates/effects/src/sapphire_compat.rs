//! Native, editable approximations of the Sapphire controls found in the SceneYard AEP corpus.
//! Parameter IDs deliberately retain the saved AE child match names. No Sapphire code is used.

use effectcraft_color::luminance;
use effectcraft_keyframe::Value;
use effectcraft_project::ParamUi;
use effectcraft_raster::{gaussian_blur, Image};
use rayon::prelude::*;

use crate::util::{fit_layer, gauss_plane, morph_frac, point_in_poly, premul, unpremul, Plane};
use crate::{col, num, popup, slider, Buf, EffectCtx, EffectSpec, ParamSpec};

fn param(prefix: &'static str, n: u16, name: &'static str, default: Value, ui: ParamUi) -> ParamSpec {
    let id: &'static str = Box::leak(format!("{prefix}-{n:04}").into_boxed_str());
    ParamSpec { id, name, default, ui }
}
fn s(prefix: &'static str, n: u16, name: &'static str, default: f64) -> ParamSpec {
    param(prefix, n, name, num(default), slider(-10000.0, 10000.0, -100.0, 100.0, 2))
}
fn c(prefix: &'static str, n: u16, name: &'static str, rgb: [f64; 3]) -> ParamSpec {
    param(prefix, n, name, col(rgb[0], rgb[1], rgb[2]), ParamUi::Color)
}
fn pt(prefix: &'static str, n: u16, name: &'static str, xy: [f64; 2]) -> ParamSpec {
    param(prefix, n, name, Value::Vec2(xy), ParamUi::Point)
}
fn spec(id: &'static str, name: &'static str, category: &'static str, params: Vec<ParamSpec>, render: crate::RenderFn) -> EffectSpec {
    EffectSpec { id, name, category, params, render, gpu: false, float: true }
}
fn f(ctx: &EffectCtx, prefix: &str, n: u16) -> f64 {
    ctx.params.f(&format!("{prefix}-{n:04}"))
}
fn color(ctx: &EffectCtx, prefix: &str, n: u16) -> [f32; 4] {
    ctx.params.color(&format!("{prefix}-{n:04}"))
}
fn point(ctx: &EffectCtx, prefix: &str, n: u16) -> [f64; 2] {
    ctx.params.v2(&format!("{prefix}-{n:04}"))
}

/// Sapphire's optional input Mask: white shows the effect, black shows the source.
/// Mocha geometry and its controls are separate and remain unsupported.
fn input_mask(ctx: &EffectCtx, id: &str, source: &Image, mut b: Buf) -> Buf {
    let layer_id = format!("{id}-0001");
    let path = f(ctx, id, 235).max(0.0).round() as usize;
    let (w, h) = (b.img.width as usize, b.img.height as usize);
    let mut matte = Plane::new(w, h);
    if let Some(layer) = ctx.layer_param(&layer_id, true) {
        let fitted = fit_layer(ctx, &b, &layer, false);
        let alpha = ctx.params.e(&format!("{id}-0540")) == 1;
        matte.data.par_iter_mut().zip(&fitted.data).for_each(|(m, p)| {
            let (rgb, a) = unpremul(*p);
            *m = if alpha { a } else { luminance(rgb[0], rgb[1], rgb[2]) * a };
        });
    } else if let Some(shape) = path.checked_sub(1).and_then(|i| ctx.env.masks.get(i)).filter(|m| m.closed && !m.points.is_empty()) {
        let inv = 1.0 / b.scale.max(1e-9);
        matte.data.par_iter_mut().enumerate().for_each(|(i, m)| {
            let x = ((i % w) as f64 + 0.5 - b.offset[0]) * inv;
            let y = ((i / w) as f64 + 0.5 - b.offset[1]) * inv;
            *m = if point_in_poly(&shape.points, x, y) != shape.inverted { 1.0 } else { 0.0 };
        });
    } else {
        return b;
    }
    let invert = ctx.params.b(&format!("{id}-0542"));
    b.img.data.par_iter_mut().zip(&source.data).zip(&matte.data).for_each(|((dst, src), m)| {
        let k = if invert { 1.0 - *m } else { *m }.clamp(0.0, 1.0);
        *dst = [0, 1, 2, 3].map(|i| src[i] * (1.0 - k) + dst[i] * k);
    });
    b
}

fn input_mask_params(id: &'static str) -> Vec<ParamSpec> {
    vec![
        param(id, 1, "Mask from Layer", Value::Layer(None), ParamUi::Layer),
        param(id, 235, "Mask from Path", num(0.0), slider(0.0, 64.0, 0.0, 16.0, 0)),
        param(id, 540, "Mask Use", Value::Enum(0), popup(&["Luma", "Alpha"])),
        param(id, 542, "Invert Mask", Value::Bool(false), ParamUi::Checkbox),
    ]
}

fn monochrome(ctx: &EffectCtx, mut b: Buf) -> Buf {
    let id = "S_Monochrome";
    let original = b.img.clone();
    let weights = [50, 51, 52].map(|n| f(ctx, id, n) as f32);
    let sum: f32 = weights.iter().sum();
    let weights = if sum.abs() > 1e-6 { weights.map(|w| w / sum) } else { [0.3, 0.5, 0.2] };
    let brightness = f(ctx, id, 53) as f32;
    let source = f(ctx, id, 54).clamp(0.0, 1.0) as f32;
    b.img.map_straight(|rgb| {
        let y = (rgb[0] * weights[0] + rgb[1] * weights[1] + rgb[2] * weights[2]) * brightness;
        [0, 1, 2].map(|i| y * (1.0 - source) + rgb[i] * source)
    });
    input_mask(ctx, id, &original, b)
}

fn gradient(ctx: &EffectCtx, mut b: Buf) -> Buf {
    let id = "S_Gradient";
    let (sx, sy) = b.to_px(point(ctx, id, 50));
    let (ex, ey) = b.to_px(point(ctx, id, 51));
    let (dx, dy) = (ex - sx, ey - sy);
    let len2 = (dx * dx + dy * dy).max(1e-9);
    let (c0, c1) = (color(ctx, id, 52), color(ctx, id, 53));
    let bright = f(ctx, id, 54) as f32;
    let smooth = f(ctx, id, 56).clamp(0.0, 1.0) as f32;
    let bg = f(ctx, id, 57) as f32;
    let combine = f(ctx, id, 58) as i32;
    let in_op = f(ctx, id, 59).clamp(0.0, 1.0) as f32;
    let out_op = f(ctx, id, 60).clamp(0.0, 1.0) as f32;
    b.img.rows_mut().for_each(|(y, row)| {
        for (x, px) in row.iter_mut().enumerate() {
            let t = ((((x as f64 + 0.5 - sx) * dx + (y as f64 + 0.5 - sy) * dy) / len2) as f32).clamp(0.0, 1.0);
            let curved = t * t * (3.0 - 2.0 * t);
            let t = t + (curved - t) * smooth;
            let g = [0, 1, 2].map(|i| (c0[i] + (c1[i] - c0[i]) * t) * bright);
            let original = *px;
            let (src, a) = unpremul(original);
            let result = match combine {
                4 => [0, 1, 2].map(|i| (src[i] * bg + g[i] * out_op).clamp(0.0, 1.0)), // Add
                _ => [0, 1, 2].map(|i| src[i] * bg * in_op * (1.0 - out_op) + g[i] * out_op),
            };
            *px = premul(result, a.max(out_op));
        }
    });
    b
}

fn drop_shadow(ctx: &EffectCtx, mut b: Buf) -> Buf {
    let id = "S_DropShadow";
    let (dx, dy) = (f(ctx, id, 53) * b.scale, f(ctx, id, 54) * b.scale);
    let blur = (f(ctx, id, 52) * b.scale).max(0.0);
    let opacity = f(ctx, id, 51).max(0.0) as f32;
    let fg = f(ctx, id, 55).clamp(0.0, 1.0) as f32;
    let clr = color(ctx, id, 50);
    if !ctx.adjustment {
        b.pad((dx.abs().max(dy.abs()) + blur * 1.5).ceil() as u32 + 2);
    }
    let src = b.img.clone();
    let mut shadow = Image::new(src.width, src.height);
    shadow.rows_mut().for_each(|(y, row)| {
        for (x, px) in row.iter_mut().enumerate() {
            let a = (src.sample_bilinear(x as f64 + 0.5 - dx, y as f64 + 0.5 - dy)[3] * opacity * clr[3]).clamp(0.0, 1.0);
            *px = [clr[0] * a, clr[1] * a, clr[2] * a, a];
        }
    });
    if blur > 0.0 {
        shadow = gaussian_blur(&shadow, blur / 2.0, blur / 2.0, false);
    }
    for (o, s) in b.img.data.iter_mut().zip(&shadow.data) {
        let front = o.map(|v| v * fg);
        for i in 0..4 {
            o[i] = front[i] + s[i] * (1.0 - front[3]);
        }
    }
    b
}

fn glow(ctx: &EffectCtx, mut b: Buf) -> Buf {
    let id = "S_Glow";
    let bright = f(ctx, id, 50) as f32;
    let threshold = f(ctx, id, 52) as f32;
    let radius = (f(ctx, id, 54) * b.scale).max(0.0);
    let wx = f(ctx, id, 55).max(0.0);
    let wy = f(ctx, id, 56).max(0.0);
    let width_rgb = [57, 58, 59].map(|n| f(ctx, id, n).max(0.0));
    let tint = color(ctx, id, 51);
    let from_alpha = f(ctx, id, 62) != 0.0;
    let source_opacity = f(ctx, id, 65).clamp(0.0, 1.0) as f32;
    if !ctx.adjustment {
        b.pad((radius * wx.max(wy) * 1.5).ceil() as u32 + 2);
    }
    let src = b.img.clone();
    let mut hot = src.clone();
    for p in &mut hot.data {
        let (rgb, a) = unpremul(*p);
        let l = if from_alpha { a } else { luminance(rgb[0], rgb[1], rgb[2]) };
        let k = ((l - threshold) / (1.0 - threshold).max(1e-4)).max(0.0);
        *p = premul([0, 1, 2].map(|i| rgb[i] * tint[i] * k), a);
    }
    let blurred = [0, 1, 2].map(|i| {
        let plane = Plane::from_image(&hot, |p| p[i]);
        gauss_plane(&plane, radius * wx * width_rgb[i] / 2.0, radius * wy * width_rgb[i] / 2.0)
    });
    for (i, p) in b.img.data.iter_mut().enumerate() {
        let glow = [0, 1, 2].map(|c| blurred[c].data[i] * bright);
        for c in 0..3 {
            p[c] = src.data[i][c] * source_opacity + glow[c];
        }
        if f(ctx, id, 61) != 0.0 {
            p[3] = (src.data[i][3] + glow.iter().copied().fold(0.0, f32::max)).clamp(0.0, 1.0);
        }
    }
    b
}

fn grid(ctx: &EffectCtx, mut b: Buf) -> Buf {
    let id = "S_Grid";
    let nx = f(ctx, id, 50).max(1.0);
    let ny = f(ctx, id, 51).max(1.0);
    let size = f(ctx, id, 52).max(1.0) * b.scale;
    let step_x = size * f(ctx, id, 53).max(1e-3) / nx;
    let step_y = size * f(ctx, id, 54).max(1e-3) / ny;
    let (sx, sy) = b.to_px(point(ctx, id, 55));
    let lw = f(ctx, id, 56).max(0.0) * b.scale;
    let major = f(ctx, id, 59).round().max(1.0) as i64;
    let major_w = f(ctx, id, 60).max(1.0);
    let h_rel = f(ctx, id, 57).max(0.0);
    let v_rel = f(ctx, id, 58).max(0.0);
    let clr = color(ctx, id, 62);
    let alpha = (f(ctx, id, 61) * f(ctx, id, 63)).clamp(0.0, 1.0) as f32;
    let roll = f(ctx, id, 66).to_radians();
    let (si, co) = roll.sin_cos();
    b.img.rows_mut().for_each(|(y, row)| {
        for (x, p) in row.iter_mut().enumerate() {
            let (ux, uy) = (x as f64 + 0.5 - sx, y as f64 + 0.5 - sy);
            let (rx, ry) = (co * ux + si * uy, -si * ux + co * uy);
            let cell_x = (rx / step_x).round() as i64;
            let cell_y = (ry / step_y).round() as i64;
            let dx = (rx - cell_x as f64 * step_x).abs();
            let dy = (ry - cell_y as f64 * step_y).abs();
            let vx = lw * v_rel * if cell_x.rem_euclid(major) == 0 { major_w } else { 1.0 };
            let hy = lw * h_rel * if cell_y.rem_euclid(major) == 0 { major_w } else { 1.0 };
            if dx <= vx / 2.0 || dy <= hy / 2.0 {
                let a = alpha * clr[3];
                for i in 0..3 {
                    p[i] = clr[i] * a + p[i] * (1.0 - a);
                }
                p[3] = a + p[3] * (1.0 - a);
            }
        }
    });
    b
}

fn halftone(ctx: &EffectCtx, mut b: Buf) -> Buf {
    let id = "S_HalfTone";
    let original = b.img.clone();
    let sigma = f(ctx, id, 56).max(0.0) * b.scale / 2.0;
    let source = if sigma > 0.0 { gaussian_blur(&original, sigma, sigma, true) } else { original.clone() };
    let dots = f(ctx, id, 51).max(1.0);
    let cell = (ctx.layer_size[0].min(ctx.layer_size[1]) * b.scale / dots).max(1.0);
    let angle = f(ctx, id, 52).to_radians();
    let (si, co) = angle.sin_cos();
    let shift_x = f(ctx, id, 59) * b.scale;
    let shift_y = f(ctx, id, 60) * b.scale;
    let c1 = color(ctx, id, 57);
    let c0 = color(ctx, id, 58);
    let lighten = f(ctx, id, 55) as f32;
    let width = (1.0 + f(ctx, id, 53)) as f32;
    b.img.rows_mut().for_each(|(y, row)| {
        for (x, p) in row.iter_mut().enumerate() {
            let (ux, uy) = (x as f64 + 0.5 + shift_x, y as f64 + 0.5 + shift_y);
            let (rx, ry) = (co * ux + si * uy, -si * ux + co * uy);
            let (fx, fy) = ((rx / cell).rem_euclid(1.0) - 0.5, (ry / cell).rem_euclid(1.0) - 0.5);
            let (rgb, a) = unpremul(source.data[y * source.width as usize + x]);
            let lum = (luminance(rgb[0], rgb[1], rgb[2]) + lighten).clamp(0.0, 1.0);
            let radius = (1.0 - lum).sqrt() * width * 0.5;
            let coverage = ((radius - fx.hypot(fy) as f32) * cell as f32).clamp(0.0, 1.0);
            *p = premul([0, 1, 2].map(|i| c1[i] * (1.0 - coverage) + c0[i] * coverage), a);
        }
    });
    b
}

fn cartoon_paint(ctx: &EffectCtx, mut b: Buf) -> Buf {
    let id = "S_CartoonPaint";
    let original = b.img.clone();
    let smooth = (f(ctx, id, 59) * b.scale / 20.0).max(0.0);
    let paint = if smooth > 0.0 { gaussian_blur(&original, smooth, smooth, true) } else { original.clone() };
    let edges = Plane::luma(&paint);
    let width = (f(ctx, id, 50) * b.scale).max(0.0);
    let strength = f(ctx, id, 51).max(0.0) as f32;
    let threshold = f(ctx, id, 52).max(0.0) as f32;
    let edge_color = color(ctx, id, 53);
    let sat = f(ctx, id, 65) as f32;
    let lights = f(ctx, id, 66) as f32;
    let darks = f(ctx, id, 67) as f32;
    let edge = edges.map(|_| 0.0);
    let edge = {
        let mut e = edge;
        for y in 0..e.h {
            for x in 0..e.w {
                let x = x as i64;
                let y = y as i64;
                let gx = edges.get_clamped(x + 1, y) - edges.get_clamped(x - 1, y);
                let gy = edges.get_clamped(x, y + 1) - edges.get_clamped(x, y - 1);
                e.data[y as usize * e.w + x as usize] = (gx * gx + gy * gy).sqrt();
            }
        }
        morph_frac(&e, width * 0.5, true)
    };
    b.img.rows_mut().for_each(|(y, row)| {
        for (x, p) in row.iter_mut().enumerate() {
            let (rgb, a) = unpremul(paint.data[y * paint.width as usize + x]);
            let l = luminance(rgb[0], rgb[1], rgb[2]);
            let mut c = [0, 1, 2].map(|i| ((l + (rgb[i] - l) * sat) * lights + darks).max(0.0));
            let k = ((edge.data[y * edge.w + x] - threshold).max(0.0) * strength).clamp(0.0, 1.0);
            for i in 0..3 {
                c[i] = c[i] * (1.0 - k) + edge_color[i] * k;
            }
            *p = premul(c, a);
        }
    });
    input_mask(ctx, id, &original, b)
}

pub fn specs() -> Vec<EffectSpec> {
    let mono = "S_Monochrome";
    let gradient = "S_Gradient";
    let shadow = "S_DropShadow";
    let glow = "S_Glow";
    let grid = "S_Grid";
    let half = "S_HalfTone";
    let paint = "S_CartoonPaint";
    vec![
        spec(
            mono,
            mono,
            "Color Correction",
            [
                vec![
                    s(mono, 50, "Weight Red", 0.3),
                    s(mono, 51, "Weight Green", 0.5),
                    s(mono, 52, "Weight Blue", 0.2),
                    s(mono, 53, "Brightness", 1.0),
                    s(mono, 54, "Mix With Source", 0.0),
                ],
                input_mask_params(mono),
            ]
            .into_iter()
            .flatten()
            .collect(),
            self::monochrome,
        ),
        spec(
            gradient,
            gradient,
            "Generate",
            vec![
                pt(gradient, 50, "Start XY", [0.0, 0.596]),
                pt(gradient, 51, "End XY", [0.0, -0.596]),
                c(gradient, 52, "Start Color", [1.0; 3]),
                c(gradient, 53, "End Color", [0.0; 3]),
                s(gradient, 54, "Brightness", 1.0),
                s(gradient, 55, "Add Noise", 0.0),
                s(gradient, 56, "Smooth Curve", 0.0),
                s(gradient, 57, "Bg Brightness", 1.0),
                s(gradient, 58, "Combine", 1.0),
                s(gradient, 59, "Input Opacity", 1.0),
                s(gradient, 60, "Output Opacity", 1.0),
            ],
            self::gradient,
        ),
        spec(
            shadow,
            shadow,
            "Perspective",
            vec![
                c(shadow, 50, "Shadow Color", [0.0; 3]),
                s(shadow, 51, "Shadow Opacity", 1.0),
                s(shadow, 52, "Shadow Blur", 10.0),
                s(shadow, 53, "Shift X", 10.0),
                s(shadow, 54, "Shift Y", 10.0),
                s(shadow, 55, "Fg Opacity", 1.0),
            ],
            self::drop_shadow,
        ),
        spec(
            glow,
            glow,
            "Stylize",
            vec![
                s(glow, 50, "Brightness", 1.0),
                c(glow, 51, "Color", [1.0; 3]),
                s(glow, 52, "Threshold", 0.5),
                c(glow, 53, "Threshold Add Color", [0.0; 3]),
                s(glow, 54, "Glow Width", 50.0),
                s(glow, 55, "Width X", 1.0),
                s(glow, 56, "Width Y", 1.0),
                s(glow, 57, "Width Red", 1.0),
                s(glow, 58, "Width Green", 1.0),
                s(glow, 59, "Width Blue", 1.0),
                s(glow, 61, "Affect Alpha", 0.0),
                s(glow, 62, "Glow From Alpha", 0.0),
                s(glow, 65, "Source Opacity", 1.0),
            ],
            self::glow,
        ),
        spec(
            grid,
            grid,
            "Generate",
            vec![
                s(grid, 50, "Boxes X", 10.0),
                s(grid, 51, "Boxes Y", 10.0),
                s(grid, 52, "Grid Size", 1000.0),
                s(grid, 53, "Grid Size X", 1.0),
                s(grid, 54, "Grid Size Y", 1.0),
                pt(grid, 55, "Shift XY", [0.5, 0.5]),
                s(grid, 56, "Line Width", 1.0),
                s(grid, 57, "H Line Rel Width", 1.0),
                s(grid, 58, "V Line Rel Width", 1.0),
                s(grid, 59, "Major Line Spacing", 5.0),
                s(grid, 60, "Major Line Width", 2.0),
                s(grid, 61, "Brightness", 1.0),
                c(grid, 62, "Color", [1.0; 3]),
                s(grid, 63, "Grid Opacity", 1.0),
                s(grid, 66, "Roll", 0.0),
            ],
            self::grid,
        ),
        spec(
            half,
            half,
            "Stylize",
            vec![
                s(half, 50, "Dots", 1.0),
                s(half, 51, "Dots Frequency", 100.0),
                s(half, 52, "Dots Angle", 0.0),
                s(half, 53, "Dots Rel Width", 0.0),
                s(half, 54, "Dots Sharpness", 0.0),
                s(half, 55, "Dots Lighten", 0.0),
                s(half, 56, "Smooth Source", 0.0),
                c(half, 57, "Color1", [1.0; 3]),
                c(half, 58, "Color0", [0.0; 3]),
                s(half, 59, "Dots Shift X", 0.0),
                s(half, 60, "Dots Shift Y", 0.0),
            ],
            self::halftone,
        ),
        spec(
            paint,
            paint,
            "Stylize",
            [
                vec![
                    s(paint, 50, "Edge Width", 1.0),
                    s(paint, 51, "Edge Strength", 1.0),
                    s(paint, 52, "Edge Threshold", 0.1),
                    c(paint, 53, "Edge Color", [0.0; 3]),
                    s(paint, 54, "Suppress Small Edges", 0.0),
                    s(paint, 55, "Edge Sharpen", 0.0),
                    s(paint, 56, "Frequency", 100.0),
                    s(paint, 57, "Stroke Length", 10.0),
                    s(paint, 58, "Stroke Align", 0.0),
                    s(paint, 59, "Smooth Colors", 0.0),
                    s(paint, 60, "Seed", 0.0),
                    s(paint, 62, "Posterize Amount", 0.0),
                    s(paint, 63, "Posterize Smooth", 0.0),
                    s(paint, 64, "Posterize Phase", 0.0),
                    s(paint, 65, "Saturation", 1.0),
                    s(paint, 66, "Scale Lights", 1.0),
                    s(paint, 67, "Offset Darks", 0.0),
                    c(paint, 68, "Tint Lights", [1.0; 3]),
                    c(paint, 69, "Tint Darks", [0.0; 3]),
                    s(paint, 70, "Opacity", 0.0),
                ],
                input_mask_params(paint),
            ]
            .into_iter()
            .flatten()
            .collect(),
            self::cartoon_paint,
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EffectEnv, EffectHost, LayerPixels, MaskShape, Params};
    use std::collections::HashMap;

    fn masked(id: &str, set: &[(&str, Value)], img: Image, masks: &[MaskShape], host: Option<&dyn EffectHost>) -> Buf {
        let spec = specs().into_iter().find(|s| s.id == id).unwrap();
        let mut values: HashMap<_, _> = spec.params.iter().map(|p| (p.id.to_string(), p.default.clone())).collect();
        for (name, value) in set {
            values.insert((*name).to_string(), value.clone());
        }
        let params = Params { values };
        let ctx = EffectCtx {
            params: &params,
            time: 0.0,
            layer_size: [img.width as f64, img.height as f64],
            seed: 0,
            adjustment: false,
            env: EffectEnv { masks, host, ..EffectEnv::default() },
        };
        crate::apply(&spec, &ctx, Buf { img, offset: [0.0; 2], scale: 1.0 })
    }

    #[test]
    fn monochrome_path_mask_and_invert() {
        let source = Image::filled(32, 32, [0.8, 0.2, 0.1, 1.0]);
        let masks = [MaskShape { name: "square".into(), points: vec![[8.0, 8.0], [24.0, 8.0], [24.0, 24.0], [8.0, 24.0]], closed: true, inverted: false }];
        let set = [("S_Monochrome-0235", num(1.0))];
        let out = masked("S_Monochrome", &set, source.clone(), &masks, None).img;
        assert_eq!(out.get(2, 2), source.get(2, 2));
        assert!((out.get(16, 16)[0] - out.get(16, 16)[1]).abs() < 1e-6);
        let inverted = masked("S_Monochrome", &[set[0].clone(), ("S_Monochrome-0542", Value::Bool(true))], source.clone(), &masks, None).img;
        assert_eq!(inverted.get(16, 16), source.get(16, 16));
        assert!((inverted.get(2, 2)[0] - inverted.get(2, 2)[1]).abs() < 1e-6);
    }

    struct WhiteLeft;
    impl EffectHost for WhiteLeft {
        fn layer(&self, id: u64, _: bool) -> Option<LayerPixels> {
            if id != 7 {
                return None;
            }
            let mut img = Image::filled(32, 32, [0.0, 0.0, 0.0, 1.0]);
            for y in 0..32 {
                for x in 0..16 {
                    img.set(x, y, [1.0; 4]);
                }
            }
            Some(LayerPixels { buf: Buf { img, offset: [0.0; 2], scale: 1.0 }, size: [32.0, 32.0] })
        }
        fn audio(&self, _: u64, _: f64, _: usize, _: u32) -> Option<Vec<f32>> {
            None
        }
    }

    #[test]
    fn cartoon_paint_layer_mask_uses_luma() {
        let source = Image::filled(32, 32, [0.8, 0.2, 0.1, 1.0]);
        let out = masked(
            "S_CartoonPaint",
            &[("S_CartoonPaint-0001", Value::Layer(Some(7))), ("S_CartoonPaint-0065", num(0.0))],
            source.clone(),
            &[],
            Some(&WhiteLeft),
        )
        .img;
        assert!((out.get(4, 16)[0] - out.get(4, 16)[1]).abs() < 1e-6);
        assert_eq!(out.get(28, 16), source.get(28, 16));
    }

    #[test]
    fn raw_ids_and_monochrome_weights() {
        let specs = specs();
        assert_eq!(specs.len(), 7);
        assert!(specs.iter().any(|s| s.id == "S_CartoonPaint" && s.params.iter().any(|p| p.id == "S_CartoonPaint-0050")));
        let values = HashMap::from([
            ("S_Monochrome-0050".into(), num(1.0)),
            ("S_Monochrome-0051".into(), num(0.0)),
            ("S_Monochrome-0052".into(), num(0.0)),
            ("S_Monochrome-0053".into(), num(1.0)),
            ("S_Monochrome-0054".into(), num(0.0)),
        ]);
        let params = Params { values };
        let ctx = EffectCtx { params: &params, time: 0.0, layer_size: [1.0, 1.0], seed: 0, adjustment: false, env: EffectEnv::default() };
        let b = monochrome(&ctx, Buf { img: Image::filled(1, 1, [0.8, 0.2, 0.1, 1.0]), offset: [0.0; 2], scale: 1.0 });
        assert_eq!(b.img.data[0], [0.8, 0.8, 0.8, 1.0]);
    }
}
