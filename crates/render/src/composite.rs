//! Compositing a 2D layer: wireframe, styled layers, track mattes and Preserve
//! Transparency, blended into the comp with the layer's mode and opacity.

use crate::color::Region;
use crate::{EvalCtx, Image, LayerTiming, Renderer, StyledLayer, styles};
use effectcraft_color::BlendMode;
use effectcraft_effects::Buf;
use effectcraft_project::{Layer, MatteKind, Quality};
use rayon::prelude::*;

impl<'a> Renderer<'a> {
    /// Draw a Wireframe-quality layer: its bounds as a one-pixel outline.
    fn draw_wireframe(&self, ctx: &EvalCtx, layer: &Layer, canvas: &mut Image) {
        for (x, y) in self.wireframe_pixels(ctx, layer, (canvas.width, canvas.height)) {
            canvas.set(x, y, [1.0, 1.0, 1.0, 1.0]);
        }
    }

    /// Draw a 2D layer into `canvas`; returns the region it may have changed.
    pub(crate) fn draw_layer(&self, ctx: &EvalCtx, layer: &Layer, canvas: &mut Image, blank: bool) -> Region {
        let opacity = ctx.opacity(layer) as f32 * self.opacity_mul;
        if self.quality(layer) == Quality::Wireframe && !layer.switches.adjustment {
            self.draw_wireframe(ctx, layer, canvas);
            return Region::Full;
        }
        if opacity <= 0.0 && !layer.switches.adjustment {
            return Region::Empty;
        }
        if layer.switches.adjustment {
            self.draw_adjustment(ctx, layer, canvas, opacity);
            return Region::Full;
        }
        let mut timing = self.profile.map(|_| LayerTiming { depth: self.depth, layer: layer.name.clone(), ..Default::default() });
        let t0 = web_time::Instant::now();
        let Some(st) = self.styled(ctx, layer, timing.as_mut()) else { return Region::Empty };
        let st = self.styled_to_blend(st);
        let t1 = web_time::Instant::now();
        let changed = if st.plain {
            self.composite_layer(ctx, layer, &st.body, canvas, opacity);
            self.footprint(ctx, layer, &st.body, opacity, blank)
        } else {
            self.composite_styled(ctx, layer, &st, canvas, opacity);
            Region::Full
        };
        if let (Some(prof), Some(mut timing)) = (self.profile, timing) {
            timing.process_ms = (t1 - t0).as_secs_f64() * 1e3;
            timing.composite_ms = t1.elapsed().as_secs_f64() * 1e3;
            if let Ok(mut v) = prof.lock() {
                v.push(timing);
            }
        }
        changed
    }

    /// Composite a styled layer: exterior passes with their own modes, then the body with the
    /// layer's mode; the layer's opacity fades the whole stack. Knockout clears what is below the
    /// layer's content; switched-off R/G/B channels keep the values below.
    fn composite_styled(&self, ctx: &EvalCtx, layer: &Layer, st: &StyledLayer, canvas: &mut Image, opacity: f32) {
        let bl = styles::blending(ctx, layer);
        let matte = layer.track_matte.is_some_and(|tm| tm.layer != layer.id && ctx.comp.layer(tm.layer).is_some());
        if matte || layer.preserve_transparency {
            let mut iso = Image::new(canvas.width, canvas.height);
            for (b, m) in &st.passes {
                self.place(ctx, layer, b, &mut iso, *m, 1.0);
            }
            self.place(ctx, layer, &st.body, &mut iso, BlendMode::Normal, 1.0);
            self.composite_iso(ctx, layer, iso, canvas, opacity);
            return;
        }
        let mut tmp = canvas.clone();
        if bl.knockout > 0 {
            let mut k = Image::new(canvas.width, canvas.height);
            self.place(ctx, layer, &st.content, &mut k, BlendMode::Normal, 1.0);
            tmp.data.par_iter_mut().zip(k.data.par_iter()).for_each(|(p, q)| {
                let keep = 1.0 - q[3].clamp(0.0, 1.0);
                for c in p.iter_mut() {
                    *c *= keep;
                }
            });
        }
        for (b, m) in &st.passes {
            self.place(ctx, layer, b, &mut tmp, *m, 1.0);
        }
        self.place(ctx, layer, &st.body, &mut tmp, layer.blend_mode, 1.0);
        let ch = bl.channels;
        canvas.data.par_iter_mut().zip(tmp.data.par_iter()).for_each(|(c, t)| {
            for i in 0..4 {
                let v = if i < 3 && !ch[i] { c[i] } else { t[i] };
                c[i] += (v - c[i]) * opacity;
            }
        });
    }

    fn composite_layer(&self, ctx: &EvalCtx, layer: &Layer, buf: &Buf, canvas: &mut Image, opacity: f32) {
        let matte = layer.track_matte.is_some_and(|tm| tm.layer != layer.id && ctx.comp.layer(tm.layer).is_some());
        if !matte && !layer.preserve_transparency {
            self.place(ctx, layer, buf, canvas, layer.blend_mode, opacity);
            return;
        }
        // Render in isolation, then matte / preserve transparency, then blend.
        let mut iso = Image::new(canvas.width, canvas.height);
        self.place(ctx, layer, buf, &mut iso, BlendMode::Normal, 1.0);
        self.composite_iso(ctx, layer, iso, canvas, opacity);
    }

    /// Apply the track matte / preserve transparency to an isolated layer render, then blend.
    pub(crate) fn composite_iso(&self, ctx: &EvalCtx, layer: &Layer, iso: Image, canvas: &mut Image, opacity: f32) {
        self.composite_iso_with(ctx, layer, iso, canvas, opacity, None);
    }

    /// [`Self::composite_iso`] with the track matte's pixels already drawn (`matte`, e.g. a 3D
    /// matte seen through the camera); otherwise the matte layer is placed in 2D.
    pub(crate) fn composite_iso_with(&self, ctx: &EvalCtx, layer: &Layer, mut iso: Image, canvas: &mut Image, opacity: f32, matte_img: Option<Image>) {
        let matte = layer.track_matte.and_then(|tm| ctx.comp.layer(tm.layer).filter(|m| m.id != layer.id).map(|m| (m, tm.kind)));
        let preserve = layer.preserve_transparency;
        if let Some((m, kind)) = matte {
            let mimg = match matte_img {
                Some(i) if i.width == canvas.width && i.height == canvas.height => i,
                _ => self.matte_image(ctx, m, canvas.width, canvas.height),
            };
            apply_matte(&mut iso, &mimg, kind);
        }
        if preserve {
            iso.data.par_iter_mut().zip(canvas.data.par_iter()).for_each(|(p, q)| {
                for c in p.iter_mut() {
                    *c *= q[3];
                }
            });
        }
        canvas.blend_from(&iso, layer.blend_mode, opacity, layer.id.0 as u32);
    }

    /// A track matte layer's pixels placed in the comp (`w`×`h`).
    pub(crate) fn matte_image(&self, ctx: &EvalCtx, m: &Layer, w: u32, h: u32) -> Image {
        let mut mimg = Image::new(w, h);
        if m.is_active_at(ctx.time)
            && let Some(mb) = self.matte_buf(ctx, m)
        {
            let mo = ctx.opacity(m) as f32;
            self.place(ctx, m, &mb, &mut mimg, BlendMode::Normal, mo);
        }
        mimg
    }
}

/// Multiply `img` by a track matte's coverage (`mimg`, same size).
pub(crate) fn apply_matte(img: &mut Image, mimg: &Image, kind: MatteKind) {
    img.data.par_iter_mut().zip(mimg.data.par_iter()).for_each(|(p, q)| {
        let k = match kind {
            MatteKind::Alpha => q[3],
            MatteKind::AlphaInverted => 1.0 - q[3],
            MatteKind::Luma => effectcraft_color::luminance(q[0], q[1], q[2]),
            MatteKind::LumaInverted => 1.0 - effectcraft_color::luminance(q[0], q[1], q[2]),
        }
        .clamp(0.0, 1.0);
        for c in p.iter_mut() {
            *c *= k;
        }
    });
}
