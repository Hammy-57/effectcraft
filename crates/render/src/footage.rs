//! Footage: the decoded-frame source, proxies, Interpret Footage (fields, alpha, linear
//! light) and frame blending.

use crate::{EvalCtx, Image, Renderer, color};
use effectcraft_effects::Buf;
use effectcraft_project::{Footage, FootageKind, FrameBlend, ItemId, ItemKind, Layer};
use effectcraft_time::{FrameRate, TICKS_PER_SECOND, Tick};
use std::sync::Arc;

/// Supplies decoded footage frames (implemented by the media layer).
pub trait FootageSource: Send + Sync {
    /// The frame of `item` at source time `t`, straight from the file (any size).
    fn frame(&self, item: ItemId, footage: &Footage, t: Tick) -> Option<Arc<Image>>;
    /// `frames` interleaved stereo sample frames of `item`'s audio from source time `t` at
    /// `rate` Hz (`None` when unavailable).
    fn audio(&self, _item: ItemId, _footage: &Footage, _t: Tick, _frames: usize, _rate: u32) -> Option<Vec<f32>> {
        None
    }
    /// Auxiliary 3D channels of `item` at source time `t` (multi-layer OpenEXR: depth, IDs,
    /// Cryptomatte, any named channel), with `scale` relative to the file's pixels. `None` when
    /// the footage has none.
    fn aux(&self, _item: ItemId, _footage: &Footage, _t: Tick) -> Option<Arc<effectcraft_raster::AuxChannels>> {
        None
    }
    /// The parsed 3D model of a [`effectcraft_project::FootageKind::Model`] item (Advanced 3D).
    fn model(&self, _item: ItemId, _footage: &Footage) -> Option<Arc<effectcraft_model::Model>> {
        None
    }
    /// Set the decoded-frame cache budget in bytes (Settings ▸ Memory & CPU); sources without
    /// a cache ignore it.
    fn set_cache_budget(&self, _bytes: usize) {}
    /// Drop the decoded frames it keeps (Edit ▸ Purge ▸ All Memory / Image Cache Memory): the
    /// next reads decode again. Sources without a cache ignore it.
    fn purge(&self) {}
    /// Settings ▸ Disk ▸ Conformed Audio Folder: where decoded audio is kept between reads
    /// (`None` = off); sources that don't decode audio ignore it.
    fn set_conform_folder(&self, _folder: Option<std::path::PathBuf>) {}
    /// The decoded-frame cache budget in bytes, if the source has a cache.
    fn cache_budget(&self) -> Option<usize> {
        None
    }
    /// Vector footage (SVG) rasterised at `scale` × its pixel size, for Continuously Rasterize.
    /// `None` when the footage is not vector or cannot be read.
    fn vector_frame(&self, _item: ItemId, _footage: &Footage, _scale: f64) -> Option<Arc<Image>> {
        None
    }
}

/// Footage that can be rasterised at any scale (SVG).
pub fn is_vector_footage(f: &Footage) -> bool {
    matches!(f.codec.as_str(), "SVG" | "PDF" | "AI" | "EPS")
}

/// No footage available (renders footage layers as transparent).
pub struct NoFootage;
impl FootageSource for NoFootage {
    fn frame(&self, _: ItemId, _: &Footage, _: Tick) -> Option<Arc<Image>> {
        None
    }
}

impl<'a> Renderer<'a> {
    /// The proxy footage used for `item` under this render's Proxy Use, if any.
    pub fn proxy_for(&self, item: ItemId) -> Option<&'a Footage> {
        let it = self.project.item(item)?;
        let px = it.proxy.as_deref()?;
        (self.opts.proxy.uses(matches!(it.kind, ItemKind::Comp(_)), px.enabled) && px.footage.has_video).then_some(&px.footage)
    }

    /// A decoded frame of `f` as a layer buffer covering `w`×`h` layer pixels (the footage's
    /// nominal size; proxies are scaled up to it), converted from its colour profile.
    pub(crate) fn footage_buf(&self, img: &Image, f: &Footage, w: u32, h: u32) -> Buf {
        let s = self.opts.scale;
        let k = if img.width > 0 { w.max(1) as f64 / img.width as f64 } else { 1.0 };
        // Keep native pixels when downsampling is small; resample otherwise.
        let mut buf = if s < 0.75 && k <= 1.0 + 1e-9 {
            let rw = ((w as f64 * s).round() as u32).max(1);
            let rh = ((h as f64 * s).round() as u32).max(1);
            Buf { img: effectcraft_raster::resample(img, rw, rh), offset: [0.0; 2], scale: s }
        } else {
            Buf { img: img.clone(), offset: [0.0; 2], scale: 1.0 / k }
        };
        if let Some(c) = self.pipe.media_in(f) {
            color::convert(&mut buf.img, &c);
        }
        buf
    }

    /// A footage frame at source time `t`, frame-blended between the two nearest source frames
    /// when the layer's Frame Blending switch (and the comp's Enable Frame Blending) is on and
    /// `t` falls between frames (footage rate ≠ comp rate, time stretch, time remapping).
    pub(crate) fn footage_frame(&self, ctx: &EvalCtx, layer: &Layer, item: ItemId, f: &Footage, t: Tick) -> Option<Arc<Image>> {
        // Interpret Footage ▸ Separate Fields: each field is a frame at twice the rate.
        if f.fields != effectcraft_project::FieldOrder::Off && matches!(f.kind, FootageKind::Video | FootageKind::Sequence) {
            let field_rate = FrameRate::new(f.frame_rate.num * 2, f.frame_rate.den);
            let i = field_rate.frame_at(t).max(0);
            let img = self.footage.frame(item, f, f.frame_rate.tick_of(i / 2))?;
            let dominant_upper = f.fields == effectcraft_project::FieldOrder::UpperFirst;
            // Upper field = even lines (0, 2, …).
            let parity = if (i % 2 == 0) == dominant_upper { 0 } else { 1 };
            return Some(Arc::new(interpret_pixels(&field_frame(&img, parity), f)));
        }
        let img = self.footage_frame_raw(ctx, layer, item, f, t)?;
        if f.invert_alpha || f.linear_light_applies() { Some(Arc::new(interpret_pixels(&img, f))) } else { Some(img) }
    }

    fn footage_frame_raw(&self, ctx: &EvalCtx, layer: &Layer, item: ItemId, f: &Footage, t: Tick) -> Option<Arc<Image>> {
        let mode = frame_blend_mode(ctx, layer);
        if mode != FrameBlend::Off && matches!(f.kind, FootageKind::Video | FootageKind::Sequence) {
            let (i, w) = frame_position(t, f.frame_rate);
            if w > 1e-6 {
                // Blended frames are cached (Pixel Motion is expensive).
                let key = self.cache.map(|_| {
                    use std::hash::{Hash, Hasher};
                    let mut h = std::collections::hash_map::DefaultHasher::new();
                    (0x00f8_a3e5_u64, item, i, (w as f32).to_bits(), mode as u8, f.frame_rate.num, f.frame_rate.den, &f.path).hash(&mut h);
                    h.finish()
                });
                if let (Some(c), Some(k)) = (self.cache, key)
                    && let Some(b) = c.get(k)
                {
                    return Some(Arc::new(b.img.clone()));
                }
                let a = self.footage.frame(item, f, f.frame_rate.tick_of(i));
                let b = self.footage.frame(item, f, f.frame_rate.tick_of(i + 1));
                if let (Some(a), Some(b)) = (a, b) {
                    let img = blend_frames(mode, &a, &b, w as f32);
                    if let (Some(c), Some(k)) = (self.cache, key) {
                        c.insert(k, Arc::new(Buf { img: img.clone(), offset: [0.0; 2], scale: 1.0 }));
                    }
                    return Some(Arc::new(img));
                }
            }
        }
        self.footage.frame(item, f, t)
    }
}

/// The layer's frame blending mode, if the comp's Enable Frame Blending switch is on.
/// One field of an interlaced frame as a full frame: the field's lines (`parity` 0 = even /
/// upper, 1 = odd / lower) kept, the other lines interpolated from their neighbours.
pub fn field_frame(img: &Image, parity: usize) -> Image {
    let (w, h) = (img.width as usize, img.height as usize);
    let mut out = img.clone();
    if h < 2 {
        return out;
    }
    for y in 0..h {
        if y % 2 == parity {
            continue;
        }
        let above = y.checked_sub(1);
        let below = (y + 1 < h).then_some(y + 1);
        for x in 0..w {
            let px = match (above, below) {
                (Some(a), Some(b)) => {
                    let (p, q) = (img.data[a * w + x], img.data[b * w + x]);
                    [(p[0] + q[0]) * 0.5, (p[1] + q[1]) * 0.5, (p[2] + q[2]) * 0.5, (p[3] + q[3]) * 0.5]
                }
                (Some(a), None) => img.data[a * w + x],
                (None, Some(b)) => img.data[b * w + x],
                (None, None) => img.data[y * w + x],
            };
            out.data[y * w + x] = px;
        }
    }
    out
}

/// Interpret Footage ▸ Invert Alpha and Interpret As Linear Light on decoded (premultiplied)
/// pixels.
pub fn interpret_pixels(img: &Image, f: &Footage) -> Image {
    let linear = f.linear_light_applies();
    if !f.invert_alpha && !linear {
        return img.clone();
    }
    let space = f.color_profile.unwrap_or(effectcraft_color::ColorSpace::Srgb);
    let mut out = img.clone();
    for p in out.data.iter_mut() {
        let a = p[3];
        let mut c = if a > 1e-6 { [p[0] / a, p[1] / a, p[2] / a] } else { [0.0; 3] };
        if linear {
            // The file's values are linear light: encode them like the rest of the footage.
            c = c.map(|v| space.encode(v));
        }
        let a = if f.invert_alpha { 1.0 - a } else { a };
        *p = [c[0] * a, c[1] * a, c[2] * a, a];
    }
    out
}

pub(crate) fn frame_blend_mode(ctx: &EvalCtx, layer: &Layer) -> FrameBlend {
    if ctx.comp.enable_frame_blending { layer.switches.frame_blend } else { FrameBlend::Off }
}

/// Position of source time `t` among frames at `rate`: the frame at or before `t` and the
/// fraction (0..1) of the way to the next frame. Exact (integer tick arithmetic), so frames that
/// line up exactly report a fraction of 0.
pub fn frame_position(t: Tick, rate: FrameRate) -> (i64, f64) {
    let num = t.0 as i128 * rate.num as i128;
    let den = TICKS_PER_SECOND as i128 * rate.den as i128;
    (num.div_euclid(den) as i64, num.rem_euclid(den) as f64 / den as f64)
}

/// Blend two neighbouring frames `w` of the way from `a` to `b`: a cross-fade (Frame Mix) or a
/// motion-compensated interpolation (Pixel Motion).
pub fn blend_frames(mode: FrameBlend, a: &Image, b: &Image, w: f32) -> Image {
    match mode {
        FrameBlend::PixelMotion => effectcraft_raster::flow::interpolate(a, b, w),
        _ => effectcraft_raster::flow::mix(a, b, w),
    }
}
