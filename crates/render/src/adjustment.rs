//! Adjustment layers: their effects run on the comp below, within their footprint.

use crate::composite::{apply_matte, window_rows, window_rows_ref};
use crate::motion_blur::MotionMemo;
use crate::{EvalCtx, Image, Renderer, color, masks};
use effectcraft_raster::pool;
use effectcraft_color::BlendMode;
use effectcraft_effects::Buf;
use effectcraft_project::Layer;
use rayon::prelude::*;

impl<'a> Renderer<'a> {
    /// Adjustment layer: effects applied to the comp below, limited to the layer's footprint
    /// (its source, masks and track matte).
    pub(crate) fn draw_adjustment(&self, ctx: &EvalCtx, layer: &Layer, canvas: &mut Image, opacity: f32) {
        let Some(mut foot) = self.source(ctx, layer) else { return };
        let _ = masks::apply(ctx, layer, &mut foot, self.mask_blur(ctx, layer));
        // The footprint only covers the window the layer reaches: the comp outside it is kept.
        let mo = MotionMemo::default();
        let win = self.reach(ctx, layer, &[&foot], &mo, (canvas.width, canvas.height));
        let mut matte = pool::zeroed(win.w, win.h);
        self.place_at(ctx, layer, &foot, &mut matte, win.origin(), BlendMode::Normal, 1.0, &mo);
        // A track matte limits where the adjustment applies (SceneYard UI Google search: a
        // tint + blur only inside a rounded card).
        if let Some((m, kind)) = self.track_matte(ctx, layer) {
            let mimg = self.matte_image_at(ctx, m, win);
            apply_matte(&mut matte, &mimg, kind);
            pool::recycle(mimg);
        }
        let o = self.roi_offset().map(|(x, y, _, _)| [-x, -y]).unwrap_or([0.0; 2]);
        // The effects' points and sizes are in the adjustment layer's space: map a moved or
        // uniformly scaled (unrotated 2D) layer onto the comp below; others keep comp space.
        let (offset, scale) = match (layer.is_3d(), ctx.layer_to_comp(layer).0.0) {
            (false, [[a, b, tx], [c, d, ty], [0.0, 0.0, 1.0]])
                if b == 0.0 && c == 0.0 && a > 0.0 && (a - d).abs() < 1e-9 && tx.is_finite() && ty.is_finite() =>
            {
                ([o[0] + tx * self.opts.scale, o[1] + ty * self.opts.scale], self.opts.scale * a)
            }
            _ => (o, self.opts.scale),
        };
        let mut below = Buf { img: pool::copy_of(canvas), offset, scale };
        // Effects run in the working space, not the linear blending space.
        if let Some(c) = self.pipe.from_blend() {
            color::convert(&mut below.img, &c);
        }
        let mut adjusted = self.apply_effects(ctx, layer, below, true);
        if adjusted.img.width != canvas.width || adjusted.img.height != canvas.height {
            // An effect grew the buffer (Motion Tile's Output Width): back to the comp.
            let Some((x, y)) = adjustment_crop(adjusted.offset, offset, (adjusted.img.width, adjusted.img.height), (canvas.width, canvas.height)) else {
                return;
            };
            adjusted.img = adjusted.img.crop(x, y, canvas.width, canvas.height);
        }
        if let Some(c) = self.pipe.to_blend() {
            color::convert(&mut adjusted.img, &c);
        }
        if adjusted.img.width != canvas.width || adjusted.img.height != canvas.height {
            return;
        }
        // A blending mode composites the adjusted comp onto the layers below with that mode
        // (SceneYard Billiard Balls: a white-to-black 4-Color Gradient adjustment in Overlay
        // shades the felt; Normal washed it out to grey).
        let mode = layer.blend_mode;
        let rows = window_rows(canvas, win).zip(window_rows_ref(&adjusted.img, win)).zip(matte.data.par_chunks(win.w.max(1) as usize));
        rows.for_each(|((crow, arow), mrow)| {
            for ((c, a), m) in crow.iter_mut().zip(arow).zip(mrow) {
                let k = m[3] * opacity;
                let a = if mode == BlendMode::Normal { *a } else { effectcraft_color::blend_pixel(mode, *c, *a, 0.5) };
                for i in 0..4 {
                    c[i] += (a[i] - c[i]) * k;
                }
            }
        });
        pool::recycle(adjusted.img);
        pool::recycle(matte);
    }
}

/// Where the comp canvas sits inside an adjustment layer's effect output that grew around it:
/// the whole-pixel shift between the output's and the input's layer origins (`offset` vs
/// `input_offset`). `None` when the shift is fractional or the canvas would not fit inside.
pub fn adjustment_crop(offset: [f64; 2], input_offset: [f64; 2], size: (u32, u32), canvas: (u32, u32)) -> Option<(i64, i64)> {
    let (dx, dy) = (offset[0] - input_offset[0], offset[1] - input_offset[1]);
    if !dx.is_finite() || !dy.is_finite() || (dx - dx.round()).abs() > 1e-6 || (dy - dy.round()).abs() > 1e-6 {
        return None;
    }
    let (x, y) = (dx.round() as i64, dy.round() as i64);
    let fits = x >= 0 && y >= 0 && x + canvas.0 as i64 <= size.0 as i64 && y + canvas.1 as i64 <= size.1 as i64;
    fits.then_some((x, y))
}
