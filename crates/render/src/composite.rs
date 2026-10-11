//! Compositing a 2D layer: wireframe, styled layers, track mattes and Preserve
//! Transparency, blended into the comp with the layer's mode and opacity.

use crate::color::Region;
use crate::motion_blur::MotionMemo;
use crate::placement::Window;
use crate::{EvalCtx, Image, LayerTiming, Renderer, StyledLayer, styles};
use effectcraft_color::BlendMode;
use effectcraft_effects::Buf;
use effectcraft_project::{Layer, MatteKind, Quality};
use effectcraft_raster::{Px, pool};
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
            // At opacity 0 the adjusted comp is mixed in at 0: nothing to run.
            if opacity <= 0.0 {
                return Region::Empty;
            }
            self.draw_adjustment(ctx, layer, canvas, opacity);
            return Region::Full;
        }
        // The layer's transforms are evaluated once for all of its buffers and steps.
        let mo = MotionMemo::default();
        if self.off_canvas(ctx, layer, (canvas.width, canvas.height), &mo) {
            return Region::Empty;
        }
        let mut timing = self.profile.map(|_| LayerTiming { depth: self.depth, layer: layer.name.clone(), ..Default::default() });
        let t0 = web_time::Instant::now();
        let Some(st) = self.styled_blend(ctx, layer, timing.as_mut()) else { return Region::Empty };
        let t1 = web_time::Instant::now();
        let changed = if st.plain {
            self.composite_layer(ctx, layer, &st.body, canvas, opacity, &mo);
            self.footprint(ctx, layer, &st.body, opacity, blank, &mo)
        } else {
            self.composite_styled(ctx, layer, &st, canvas, opacity, &mo)
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
    ///
    /// The temporaries only cover the window the passes, body (and knockout shape) reach; stencil
    /// modes, which change the comp outside them, use the whole canvas.
    /// Returns the region it changed.
    fn composite_styled(&self, ctx: &EvalCtx, layer: &Layer, st: &StyledLayer, canvas: &mut Image, opacity: f32, mo: &MotionMemo) -> Region {
        let bl = styles::blending(ctx, layer);
        let matte = layer.track_matte.is_some_and(|tm| tm.layer != layer.id && ctx.comp.layer(tm.layer).is_some());
        let size = (canvas.width, canvas.height);
        let whole = Window { x: 0, y: 0, w: size.0, h: size.1 };
        let mut bufs: Vec<&Buf> = st.passes.iter().map(|(b, _)| &**b).collect();
        bufs.push(&st.body);
        if matte || layer.preserve_transparency {
            // Stencil passes only clear the (transparent) isolated buffer; the layer's own
            // stencil mode reaches the whole comp.
            let win = if layer.blend_mode.is_stencil() { whole } else { self.reach(ctx, layer, &bufs, mo, size) };
            if win.is_empty() {
                return Region::Empty;
            }
            let mut iso = pool::zeroed(win.w, win.h);
            for (b, m) in &st.passes {
                self.place_at(ctx, layer, b, &mut iso, win.origin(), *m, 1.0, mo);
            }
            self.place_at(ctx, layer, &st.body, &mut iso, win.origin(), BlendMode::Normal, 1.0, mo);
            self.composite_iso_at(ctx, layer, iso, win, canvas, opacity, None);
            return win.region(size);
        }
        if bl.knockout > 0 {
            bufs.push(&st.content);
        }
        let stencil = layer.blend_mode.is_stencil() || st.passes.iter().any(|(_, m)| m.is_stencil());
        let win = if stencil { whole } else { self.reach(ctx, layer, &bufs, mo, size) };
        if win.is_empty() {
            return Region::Empty;
        }
        let mut tmp = pool::crop_of(canvas, win.x, win.y, win.w, win.h);
        if bl.knockout > 0 {
            let mut k = pool::zeroed(win.w, win.h);
            self.place_at(ctx, layer, &st.content, &mut k, win.origin(), BlendMode::Normal, 1.0, mo);
            tmp.data.par_iter_mut().zip(k.data.par_iter()).for_each(|(p, q)| {
                let keep = 1.0 - q[3].clamp(0.0, 1.0);
                for c in p.iter_mut() {
                    *c *= keep;
                }
            });
            pool::recycle(k);
        }
        for (b, m) in &st.passes {
            self.place_at(ctx, layer, b, &mut tmp, win.origin(), *m, 1.0, mo);
        }
        self.place_at(ctx, layer, &st.body, &mut tmp, win.origin(), layer.blend_mode, 1.0, mo);
        let ch = bl.channels;
        window_rows(canvas, win).zip(tmp.data.par_chunks(win.w as usize)).for_each(|(crow, trow)| {
            for (c, t) in crow.iter_mut().zip(trow) {
                for i in 0..4 {
                    let v = if i < 3 && !ch[i] { c[i] } else { t[i] };
                    c[i] += (v - c[i]) * opacity;
                }
            }
        });
        pool::recycle(tmp);
        win.region(size)
    }

    fn composite_layer(&self, ctx: &EvalCtx, layer: &Layer, buf: &Buf, canvas: &mut Image, opacity: f32, mo: &MotionMemo) {
        let matte = layer.track_matte.is_some_and(|tm| tm.layer != layer.id && ctx.comp.layer(tm.layer).is_some());
        if !matte && !layer.preserve_transparency {
            self.place_memo(ctx, layer, buf, canvas, layer.blend_mode, opacity, mo);
            return;
        }
        // Render in isolation (as far as the layer reaches; a stencil mode reaches the whole
        // comp), then matte / preserve transparency, then blend.
        let size = (canvas.width, canvas.height);
        let win = if layer.blend_mode.is_stencil() { Window { x: 0, y: 0, w: size.0, h: size.1 } } else { self.reach(ctx, layer, &[buf], mo, size) };
        if win.is_empty() {
            return;
        }
        let mut iso = pool::zeroed(win.w, win.h);
        self.place_at(ctx, layer, buf, &mut iso, win.origin(), BlendMode::Normal, 1.0, mo);
        self.composite_iso_at(ctx, layer, iso, win, canvas, opacity, None);
    }

    /// Apply the track matte / preserve transparency to an isolated layer render, then blend.
    pub(crate) fn composite_iso(&self, ctx: &EvalCtx, layer: &Layer, iso: Image, canvas: &mut Image, opacity: f32) {
        self.composite_iso_with(ctx, layer, iso, canvas, opacity, None);
    }

    /// [`Self::composite_iso`] with the track matte's pixels already drawn (`matte`, e.g. a 3D
    /// matte seen through the camera); otherwise the matte layer is placed in 2D.
    pub(crate) fn composite_iso_with(&self, ctx: &EvalCtx, layer: &Layer, iso: Image, canvas: &mut Image, opacity: f32, matte_img: Option<Image>) {
        let win = Window { x: 0, y: 0, w: canvas.width, h: canvas.height };
        self.composite_iso_at(ctx, layer, iso, win, canvas, opacity, matte_img);
    }

    /// [`Self::composite_iso_with`] for an isolated render of the window `win` of the canvas
    /// (`iso` is `win`'s size; `matte_img`, if any, the whole canvas's).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn composite_iso_at(
        &self,
        ctx: &EvalCtx,
        layer: &Layer,
        mut iso: Image,
        win: Window,
        canvas: &mut Image,
        opacity: f32,
        matte_img: Option<Image>,
    ) {
        let matte = layer.track_matte.and_then(|tm| ctx.comp.layer(tm.layer).filter(|m| m.id != layer.id).map(|m| (m, tm.kind)));
        let preserve = layer.preserve_transparency;
        let size = (canvas.width, canvas.height);
        if let Some((m, kind)) = matte {
            let mimg = match matte_img {
                Some(i) if i.width == canvas.width && i.height == canvas.height && win.is_whole(size) => i,
                Some(i) if i.width == canvas.width && i.height == canvas.height => {
                    let c = pool::crop_of(&i, win.x, win.y, win.w, win.h);
                    pool::recycle(i);
                    c
                }
                _ => self.matte_image_at(ctx, m, win),
            };
            apply_matte(&mut iso, &mimg, kind);
            pool::recycle(mimg);
        }
        if preserve {
            iso.data.par_chunks_mut(win.w.max(1) as usize).zip(window_rows_ref(canvas, win)).for_each(|(row, crow)| {
                for (p, q) in row.iter_mut().zip(crow) {
                    for c in p.iter_mut() {
                        *c *= q[3];
                    }
                }
            });
        }
        // Dissolve noise follows the output's pixels (the canvas is its window at the origin).
        let [ox, oy] = self.canvas_origin;
        let at = [win.x.saturating_add(ox), win.y.saturating_add(oy)];
        canvas.blend_from_at(self.canvas_origin, &iso, at, layer.blend_mode, opacity, layer.id.0 as u32);
        pool::recycle(iso);
    }

    /// A track matte layer's pixels placed in the window `win` of the comp.
    pub(crate) fn matte_image_at(&self, ctx: &EvalCtx, m: &Layer, win: Window) -> Image {
        let mut mimg = pool::zeroed(win.w, win.h);
        if m.is_active_at(ctx.time)
            && let Some(mb) = self.matte_buf(ctx, m)
        {
            let mo = ctx.opacity(m) as f32;
            self.place_at(ctx, m, &mb, &mut mimg, win.origin(), BlendMode::Normal, mo, &MotionMemo::default());
        }
        mimg
    }
}

/// The rows of the window `win` of `canvas` (each `win.w` pixels), in parallel.
pub(crate) fn window_rows(canvas: &mut Image, win: Window) -> impl IndexedParallelIterator<Item = &mut [Px]> {
    let (cw, x0, x1) = (canvas.width.max(1) as usize, win.x as usize, win.x as usize + win.w as usize);
    canvas.data.par_chunks_mut(cw).skip(win.y as usize).take(win.h as usize).map(move |r| r.get_mut(x0..x1).unwrap_or_default())
}

/// [`window_rows`] for reading.
pub(crate) fn window_rows_ref(canvas: &Image, win: Window) -> impl IndexedParallelIterator<Item = &[Px]> {
    let (cw, x0, x1) = (canvas.width.max(1) as usize, win.x as usize, win.x as usize + win.w as usize);
    canvas.data.par_chunks(cw).skip(win.y as usize).take(win.h as usize).map(move |r| r.get(x0..x1).unwrap_or_default())
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
