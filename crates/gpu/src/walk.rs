//! The per-frame compositing walk on the GPU: the same steps as `Renderer::draw_comp` (layer
//! order, 3D runs, collapsed precomps, track mattes, Preserve Transparency, layer styles, motion
//! blur, bit-depth quantisation, colour conversions), with layer pixels taken from the CPU
//! renderer and its layer cache. Classic and Advanced 3D runs (environment skies included) and
//! adjustment layers run on the GPU; steps without a GPU implementation (anything a kernel
//! declines) read the canvas back, run on the CPU and upload the result.

use std::sync::Arc;

use effectcraft_color::BlendMode;
use effectcraft_effects::Buf;
use effectcraft_project::{ItemId, Layer, Quality};
use effectcraft_raster::Image;
use effectcraft_render::{EvalCtx, Renderer, styles};
use effectcraft_time::Tick;

use crate::context::{Enc, GpuImage};
use crate::ops;

/// Render a comp frame into a GPU image (the canvas in the output space, as
/// `Renderer::comp_frame_cpu` returns it). `None` when something cannot run here (the caller
/// renders on the CPU instead).
pub(crate) fn render<'g>(e: &mut Enc<'g>, r: &Renderer, comp_id: ItemId, t: Tick) -> Option<GpuImage> {
    // Region-of-interest frames are cropped on the CPU path.
    if r.opts.roi.is_some() {
        return None;
    }
    let ctx = r.eval_ctx(comp_id, t)?;
    let s = r.opts.scale;
    let w = ((ctx.comp.width as f64 * s).round() as u32).max(1);
    let h = ((ctx.comp.height as f64 * s).round() as u32).max(1);
    if r.depth() > 16 || !e.g.fits(w, h) {
        return None;
    }
    let mut canvas = e.zeros(w, h);
    draw_comp(e, r, &ctx, &mut canvas)?;
    let pipe = r.pipe();
    if let Some(c) = pipe.from_blend() {
        canvas = ops::convert(e, &canvas, &c);
    }
    if let Some(l) = pipe.levels {
        canvas = ops::quantize(e, &canvas, l);
    }
    if r.depth() == 0
        && let Some(c) = pipe.output()
    {
        canvas = ops::convert(e, &canvas, &c);
        if let Some(l) = pipe.levels {
            canvas = ops::quantize(e, &canvas, l);
        }
    }
    if r.depth() == 0
        && let Some((lin, mode, enc)) = pipe.output_hdr()
    {
        on_cpu(e, &mut canvas, |img| {
            effectcraft_render::color::output_hdr(img, lin, mode, enc);
            pipe.quantize(img);
        })?;
    }
    Some(canvas)
}

/// Run a CPU drawing step on the canvas (read back, draw, upload).
fn on_cpu(e: &mut Enc, canvas: &mut GpuImage, f: impl FnOnce(&mut Image)) -> Option<()> {
    let mut img = e.download(canvas)?;
    f(&mut img);
    *canvas = e.g.upload_image(&img)?;
    Some(())
}

fn draw_comp<'a>(e: &mut Enc, r: &Renderer<'a>, ctx: &EvalCtx<'a>, canvas: &mut GpuImage) -> Option<()> {
    // Passes as on the CPU (Posterize Time adjustment layers hold what is below them).
    for (ctx, visible) in r.draw_passes(ctx) {
        draw_layers(e, r, &ctx, &visible, canvas)?;
    }
    Some(())
}

fn draw_layers<'a>(e: &mut Enc, r: &Renderer<'a>, ctx: &EvalCtx<'a>, visible: &[&'a Layer], canvas: &mut GpuImage) -> Option<()> {
    let levels = r.pipe().levels;
    // The layers' content on every core first (`prepass`); dropped with what the walk didn't take.
    let _prefetched = crate::prepass::run(e, r, ctx, visible, canvas, ordinary_resident_enabled());
    let mut i = 0;
    while i < visible.len() {
        if visible[i].is_3d() {
            let mut j = i;
            while j < visible.len() && visible[j].is_3d() {
                j += 1;
            }
            draw_3d(e, r, ctx, &visible[i..j], canvas)?;
            i = j;
        } else {
            match (r.collapsed(ctx, visible[i]), r.collapsed_fx(ctx, visible[i])) {
                (Some(item), _) => draw_collapsed(e, r, ctx, visible[i], item, canvas)?,
                (None, Some(item)) => {
                    if draw_collapsed_fx(e, r, ctx, visible[i], item, canvas).is_none() {
                        on_cpu(e, canvas, |img| {
                            r.draw_collapsed_fx(ctx, visible[i], item, img);
                        })?;
                    }
                }
                (None, None) => draw_layer(e, r, ctx, visible[i], canvas)?,
            }
            i += 1;
        }
        // 8/16 bpc: the comp is an integer buffer after every layer.
        if let Some(l) = levels {
            *canvas = ops::quantize(e, canvas, l);
        }
    }
    Some(())
}

/// A run of consecutive 3D layers: Classic 3D on the GPU (`classic3d`), with 3D adjustment
/// layers splitting the run as on the CPU; Advanced 3D, wireframes and runs the kernel cannot
/// hold draw on the CPU.
fn draw_3d<'a>(e: &mut Enc, r: &Renderer<'a>, ctx: &EvalCtx<'a>, run: &[&'a Layer], canvas: &mut GpuImage) -> Option<()> {
    // Environment Light Background layers: the sky behind the rest of the run (drawn first, as
    // `three_d::compose::draw_run` does).
    if run.iter().any(|l| l.environment_background) {
        for l in run.iter().filter(|l| l.environment_background) {
            if let Some(sky) = r.sky_draw(ctx, l) {
                *canvas = crate::adv3d::sky(e, &sky, canvas)?;
            }
        }
        let rest: Vec<&'a Layer> = run.iter().copied().filter(|l| !l.environment_background).collect();
        return draw_3d(e, r, ctx, &rest, canvas);
    }
    if let Some(k) = run.iter().position(|l| l.switches.adjustment) {
        draw_3d(e, r, ctx, &run[..k], canvas)?;
        draw_layer(e, r, ctx, run[k], canvas)?;
        return draw_3d(e, r, ctx, &run[k + 1..], canvas);
    }
    if let Some((k, item)) = run.iter().enumerate().find_map(|(k, l)| r.collapsed_fx(ctx, l).map(|item| (k, item))) {
        draw_3d(e, r, ctx, &run[..k], canvas)?;
        on_cpu(e, canvas, |img| {
            r.draw_collapsed_fx(ctx, run[k], item, img);
        })?;
        return draw_3d(e, r, ctx, &run[k + 1..], canvas);
    }
    if run.is_empty() {
        return Some(());
    }
    // Advanced 3D: rendered and composited on the GPU (`adv3d`).
    if let Some(prep) = r.prepare_adv_run(ctx, run, (canvas.width, canvas.height)) {
        match crate::adv3d::draw_run(e, &prep, canvas) {
            Some(img) => *canvas = img,
            None => on_cpu(e, canvas, |img| r.draw_3d_run(ctx, run, img))?,
        }
        return Some(());
    }
    // Advanced 3D with layers on the 2D compositing path (blend modes, track mattes, Preserve
    // Transparency).
    if let Some(split) = r.split_adv_run(ctx, run, (canvas.width, canvas.height)) {
        match draw_split(e, r, ctx, &split, canvas) {
            Some(img) => *canvas = img,
            None => on_cpu(e, canvas, |img| r.draw_3d_run(ctx, run, img))?,
        }
        return Some(());
    }
    match r.prepare_3d_run(ctx, run, (canvas.width, canvas.height)).and_then(|prep| crate::classic3d::draw_run(e, &prep, canvas)) {
        Some(img) => *canvas = img,
        None => on_cpu(e, canvas, |img| r.draw_3d_run(ctx, run, img))?,
    }
    Some(())
}

/// `three_d::adv::draw_run`'s split path on the GPU canvas: the plain layers as one scene over
/// the canvas, then each special layer (far to near) rendered alone, hidden where the main
/// scene is nearer, matted (a 3D matte through the camera, else the 2D placement), Preserve
/// Transparency and blended. `None` = render the run on the CPU.
fn draw_split(e: &mut Enc, r: &Renderer, ctx: &EvalCtx, split: &effectcraft_render::three_d::adv::SplitRun, canvas: &GpuImage) -> Option<GpuImage> {
    let (w, h) = (canvas.width, canvas.height);
    let main_prep = split.main();
    if main_prep.out != (w, h) {
        return None;
    }
    let encode = !main_prep.linear;
    let main = crate::adv3d::resolve_run(e, &main_prep)?;
    let mut cv = match &main {
        Some(m) => crate::adv3d::finish(e, m, encode, Some(canvas)),
        None => canvas.clone(),
    };
    for i in 0..split.len() {
        let Some(iso) = crate::adv3d::resolve_run(e, &split.special(i))? else { continue };
        let iso = match &main {
            Some(m) => crate::adv3d::occluded(e, &iso, m, encode),
            None => crate::adv3d::finish(e, &iso, encode, None),
        };
        let matte3d = split.with_matte3d(i, |p| match p {
            None => Some(e.zeros(w, h)),
            Some(p) => Some(match crate::adv3d::resolve_run(e, p)? {
                Some(m) => crate::adv3d::finish(e, &m, encode, None),
                None => e.zeros(w, h),
            }),
        });
        let matte3d = match matte3d {
            Some(m) => Some(m?),
            None => None,
        };
        composite_iso_with(e, r, ctx, split.layer(i), iso, &mut cv, 1.0, matte3d, None)?;
    }
    Some(cv)
}

fn draw_collapsed<'a>(e: &mut Enc, r: &Renderer<'a>, ctx: &EvalCtx<'a>, layer: &Layer, item: ItemId, canvas: &mut GpuImage) -> Option<()> {
    let opacity = r.layer_opacity(ctx, layer);
    if opacity <= 0.0 {
        return Some(());
    }
    let matte = r.track_matte(ctx, layer).is_some();
    if layer.blend_mode == BlendMode::Normal && !matte && !layer.preserve_transparency {
        if let Some((sub, nctx)) = r.collapse_into(ctx, layer, item, opacity) {
            draw_comp(e, &sub, &nctx, canvas)?;
        }
        return Some(());
    }
    let Some((sub, nctx)) = r.collapse_into(ctx, layer, item, 1.0) else { return Some(()) };
    let mut iso = e.zeros(canvas.width, canvas.height);
    draw_comp(e, &sub, &nctx, &mut iso)?;
    composite_iso(e, r, ctx, layer, iso, canvas, opacity)
}

/// Collapsed 2D contents are already placed in comp space before the parent
/// effect stack. Keep that isolation resident; unsupported work keeps CPU fallback.
fn draw_collapsed_fx<'a>(e: &mut Enc, r: &Renderer<'a>, ctx: &EvalCtx<'a>, layer: &'a Layer, item: ItemId, canvas: &mut GpuImage) -> Option<()> {
    let opacity = r.layer_opacity(ctx, layer);
    if opacity <= 0.0 {
        return Some(());
    }
    let Some((sub, nctx)) = r.collapse_into(ctx, layer, item, 1.0) else {
        return Some(());
    };
    let mut iso = e.zeros(canvas.width, canvas.height);
    // collapse_into propagates the parent transform and motion-blur time chain.
    draw_comp(e, &sub, &nctx, &mut iso)?;
    let pipe = r.pipe();
    if let Some(c) = pipe.from_blend() {
        iso = ops::convert(e, &iso, &c);
    }
    let mut fx = crate::effects::GpuFx::new(e, iso, r.collapsed_effect_offset(), r.opts.scale, pipe.levels);
    r.run_effects_on(ctx, layer, true, &mut fx);
    let (mut done, _, _) = fx.finish()?;
    if let Some(c) = pipe.to_blend() {
        done = ops::convert(e, &done, &c);
    }
    // Match draw_collapsed_fx: an effect that changes the canvas extent draws nothing.
    if (done.width, done.height) != (canvas.width, canvas.height) {
        return Some(());
    }
    composite_iso(e, r, ctx, layer, done, canvas, opacity)
}

fn draw_layer(e: &mut Enc, r: &Renderer, ctx: &EvalCtx, layer: &Layer, canvas: &mut GpuImage) -> Option<()> {
    if layer.switches.adjustment {
        if draw_adjustment(e, r, ctx, layer, canvas).is_some() {
            return Some(());
        }
        return on_cpu(e, canvas, |img| r.draw_layer_cpu(ctx, layer, img));
    }
    if r.quality(layer) == Quality::Wireframe {
        // The layer's bounds as a one-pixel outline (the CPU's pixels).
        *canvas = crate::adv3d::wireframe(e, canvas, &r.wireframe_pixels(ctx, layer, (canvas.width, canvas.height)));
        return Some(());
    }
    let opacity = r.layer_opacity(ctx, layer);
    if opacity <= 0.0 {
        return Some(());
    }
    if let Some(done) = draw_precomp(e, r, ctx, layer, canvas, opacity) {
        return done;
    }
    if let Some(done) = draw_yuv_footage(e, r, ctx, layer, canvas, opacity) {
        return done;
    }
    if let Some(done) = draw_ordinary_resident(e, r, ctx, layer, canvas, opacity) {
        return done;
    }
    let Some(st) = r.styled_layer(ctx, layer) else { return Some(()) };
    if st.plain {
        return composite_layer(e, r, ctx, layer, &st.body, canvas, opacity);
    }
    // Layer styles: exterior passes with their own modes, then the body; the layer's opacity
    // fades the whole stack; knockout and channel switches as on the CPU.
    let bl = styles::blending(ctx, layer);
    let (w, h) = (canvas.width, canvas.height);
    if r.track_matte(ctx, layer).is_some() || layer.preserve_transparency {
        let mut iso = e.zeros(w, h);
        for (b, m) in &st.passes {
            iso = place(e, r, ctx, layer, b, &iso, *m, 1.0, None)?;
        }
        iso = place(e, r, ctx, layer, &st.body, &iso, BlendMode::Normal, 1.0, None)?;
        return composite_iso(e, r, ctx, layer, iso, canvas, opacity);
    }
    let mut tmp = canvas.clone();
    if bl.knockout > 0 {
        let k = place(e, r, ctx, layer, &st.content, &e.zeros(w, h), BlendMode::Normal, 1.0, None)?;
        tmp = ops::knockout(e, &tmp, &k);
    }
    for (b, m) in &st.passes {
        tmp = place(e, r, ctx, layer, b, &tmp, *m, 1.0, None)?;
    }
    tmp = place(e, r, ctx, layer, &st.body, &tmp, layer.blend_mode, 1.0, None)?;
    *canvas = ops::channel_mix(e, canvas, &tmp, bl.channels, opacity);
    Some(())
}

// Profile-only experiment: disable the ordinary resident producer without
// changing shared ownership or native YUV caching. Read once per process.
#[cfg(feature = "profile")]
fn ordinary_resident_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var("EFFECTCRAFT_GPU_RESIDENT").map_or(true, |value| value != "0"))
}

#[cfg(not(feature = "profile"))]
fn ordinary_resident_enabled() -> bool {
    true
}

// Profile-only experiment: `EFFECTCRAFT_RT_DISABLE_PRECOMP_RESIDENT=1` composites every
// precomp again each frame (the behaviour before static precomps were retained).
#[cfg(feature = "profile")]
fn precomp_resident_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var("EFFECTCRAFT_RT_DISABLE_PRECOMP_RESIDENT").map_or(true, |value| value != "1"))
}

#[cfg(not(feature = "profile"))]
fn precomp_resident_enabled() -> bool {
    true
}

/// Ordinary plain layers retain layer-space Processed pixels. Styles and precomps
/// keep their existing paths; keys declined by dependency analysis are not retained.
fn draw_ordinary_resident(e: &mut Enc, r: &Renderer, ctx: &EvalCtx, layer: &Layer, canvas: &mut GpuImage, opacity: f32) -> Option<Option<()>> {
    use effectcraft_render::cache::{Claim, GpuStage};
    if !ordinary_resident_enabled()
        || !e.can_retain_resident()
        || styles::active(ctx, layer)
        || matches!(layer.source, effectcraft_project::LayerSource::Comp { .. })
    {
        return None;
    }
    let cache = r.cache?;
    let (key, clip) = {
        let _profile = effectcraft_render::profile::span("ordinary_resident_key");
        r.resident_content(ctx, layer)?
    };
    let resident = {
        let _profile = effectcraft_render::profile::span("ordinary_resident_lookup");
        e.resident(key, GpuStage::Processed)
    };
    if let Some(image) = resident {
        let _profile = effectcraft_render::profile::span("ordinary_resident_hit");
        return composite_processed(e, r, ctx, layer, canvas, opacity, image);
    }
    // Reuse ordinary CPU/disk hits. An image from another device is materialized
    // through its own guarded callback, never bound to this encoder's device.
    let intent = cache.disk_intent(key);
    let mut cpu = cache.get(key);
    let mut flight = None;
    if cpu.is_none() {
        match cache.claim_gpu(key) {
            Claim::Ready(buf) => cpu = Some(buf),
            Claim::Owner(guard) => flight = Some(guard),
            Claim::Render => {
                // A GPU-only producer wakes the same flight without a CPU cache entry.
                if let Some(image) = e.resident(key, GpuStage::Processed) {
                    return composite_processed(e, r, ctx, layer, canvas, opacity, image);
                }
                return None;
            }
        }
    }
    let image = if let Some(buf) = cpu {
        crate::context::NativeImage { img: e.upload_buf(&buf)?, offset: buf.offset, scale: buf.scale }
    } else {
        let _profile = effectcraft_render::profile::span("ordinary_resident_produce");
        let buf = crate::prepass::resident_source(r, ctx, layer, key, clip)?;
        let src = e.g.upload_image(&buf.img)?;
        let mut fx = crate::effects::GpuFx::new(e, src, buf.offset, buf.scale, r.pipe().levels);
        let origin = r.run_layer_effects_on(ctx, layer, &mut fx);
        let (img, offset, scale) = fx.finish()?;
        crate::context::NativeImage { img, offset: [offset[0] - origin[0] * scale, offset[1] - origin[1] * scale], scale }
    };
    let result = composite_processed(e, r, ctx, layer, canvas, opacity, image.clone());
    if result == Some(Some(()))
        && let Some(flight) = flight
    {
        // Unique animated versions are transient. Held values and repeated visits
        // can reuse their exact key after the bounded doorkeeper sees it twice.
        if r.resident_content_animated(ctx, layer) && !cache.admit_repeated_gpu_content(key) {
            return result;
        }
        e.keep_resident(key, image, flight, intent);
    }
    result
}

#[allow(clippy::too_many_arguments)]
fn composite_processed(
    e: &mut Enc,
    r: &Renderer,
    ctx: &EvalCtx,
    layer: &Layer,
    canvas: &mut GpuImage,
    opacity: f32,
    mut image: crate::context::NativeImage,
) -> Option<Option<()>> {
    if let Some(conversion) = r.pipe().to_blend() {
        image.img = ops::convert(e, &image.img, &conversion);
    }
    composite_resident(e, r, ctx, layer, canvas, opacity, image)
}

/// A 2D precomp layer, its nested comp rendered resident on the GPU (`walk::render` one level
/// down) instead of by the CPU compositor and uploaded every frame (`Renderer::layer_source`
/// renders nested comps on the CPU: depth > 0). Then the layer's effects (resident), its
/// placement, track matte / Preserve Transparency and blend, as for any layer. `None`: not this
/// kind of layer, or the nested comp needs the CPU path (masks, layer styles, proxies,
/// overrides, frame blending, nested motion-blur sampling; see `Renderer::precomp_source`);
/// `Some(None)`: the GPU failed.
fn draw_precomp(e: &mut Enc, r: &Renderer, ctx: &EvalCtx, layer: &Layer, canvas: &mut GpuImage, opacity: f32) -> Option<Option<()>> {
    if layer.masks().is_some_and(|m| !m.children.is_empty()) || styles::active(ctx, layer) {
        return None;
    }
    let source = r.precomp_source(ctx, layer)?;
    if let Some(done) = draw_cached_precomp(e, r, ctx, layer, canvas, opacity, &source) {
        return Some(done);
    }
    let (sub, item, lt, bs) = source;
    // A nested comp the GPU can't take (nested too deep, over the texture limit, content it
    // declines) goes back to the CPU path for this layer only; only a device failure fails the
    // frame.
    let Some(src) = render(e, &sub, item, lt) else {
        return if e.g.check_health().is_ok() { None } else { Some(None) };
    };
    finish_resident(e, r, ctx, layer, canvas, opacity, src, bs)
}

/// Reuse completed layer-space precomp pixels, never the parent placement or blend.
/// `None` preserves the existing uncached precomp/CPU fallback.
fn draw_cached_precomp(
    e: &mut Enc,
    r: &Renderer,
    ctx: &EvalCtx,
    layer: &Layer,
    canvas: &mut GpuImage,
    opacity: f32,
    source: &(Renderer<'_>, ItemId, Tick, f64),
) -> Option<Option<()>> {
    use effectcraft_render::cache::{Claim, GpuStage};
    if !precomp_resident_enabled() || !e.can_retain_resident() {
        return None;
    }
    let cache = r.cache?;
    let (key, noted) = {
        let _profile = effectcraft_render::profile::span("precomp_resident_key");
        r.resident_precomp_key(ctx, layer)?
    };
    if let Some(image) = e.resident(key, GpuStage::Processed) {
        let _profile = effectcraft_render::profile::span("precomp_resident_hit");
        r.resident_precomp_note(noted, true);
        return composite_processed(e, r, ctx, layer, canvas, opacity, image);
    }
    let intent = cache.disk_intent(key);
    let mut cpu = cache.get(key);
    let mut flight = None;
    if cpu.is_none() {
        match cache.claim_gpu(key) {
            Claim::Ready(buf) => cpu = Some(buf),
            Claim::Owner(guard) => flight = Some(guard),
            Claim::Render => {
                if let Some(image) = e.resident(key, GpuStage::Processed) {
                    r.resident_precomp_note(noted, true);
                    return composite_processed(e, r, ctx, layer, canvas, opacity, image);
                }
                r.resident_precomp_note(noted, false);
                return None;
            }
        }
    }
    r.resident_precomp_note(noted, cpu.is_some());
    let image = if let Some(buf) = cpu {
        crate::context::NativeImage { img: e.upload_buf(&buf)?, offset: buf.offset, scale: buf.scale }
    } else {
        let _profile = effectcraft_render::profile::span("precomp_resident_produce");
        let (sub, item, lt, bs) = source;
        let Some(src) = render(e, sub, *item, *lt) else {
            return if e.g.check_health().is_ok() { None } else { Some(None) };
        };
        prepare_processed(e, r, ctx, layer, src, *bs)?
    };
    let result = composite_processed(e, r, ctx, layer, canvas, opacity, image.clone());
    if result == Some(Some(()))
        && let Some(flight) = flight
    {
        e.keep_resident(key, image, flight, intent);
    }
    result
}

fn draw_yuv_footage(e: &mut Enc, r: &Renderer, ctx: &EvalCtx, layer: &Layer, canvas: &mut GpuImage, opacity: f32) -> Option<Option<()>> {
    if !crate::yuv_transport_enabled() || e.g.yuv.is_none() || styles::active(ctx, layer) {
        return None;
    }
    let footage = {
        let _profile = effectcraft_render::profile::span("native_source_lookup");
        r.native_footage(ctx, layer)?
    };
    let view = footage.pixels.yuv()?;
    let metadata = crate::yuv::metadata(view);
    let cache = {
        let _profile = effectcraft_render::profile::span("native_content_key");
        r.resident_footage_key(ctx, layer)
    };
    if let Some((key, token)) = cache.as_ref()
        && let Some(image) = e.g.native_get(&footage.pixels, *key, token, &metadata, e.owner.as_ref())
    {
        let _profile = effectcraft_render::profile::span("native_cached_place");
        return composite_resident(e, r, ctx, layer, canvas, opacity, image);
    }
    let mut src = crate::yuv::decode(e, view)?;
    if let Some((w, h)) = footage.preview {
        if !e.g.fits(w, h) {
            return None;
        }
        let matrix = effectcraft_geom::Mat3::scale(effectcraft_geom::vec2(w as f64 / src.width as f64, h as f64 / src.height as f64));
        src = ops::warp(e, &e.zeros(w, h), &src, &matrix, effectcraft_raster::Sampling::Bilinear, BlendMode::Normal, 1.0, 0, None);
    }
    if let Some(c) = footage.conversion {
        src = ops::convert(e, &src, &c);
    }
    let geometry = effectcraft_render::masks::Geometry { width: src.width, height: src.height, offset: [0.0; 2], scale: footage.scale };
    if let Some(mask) = r.source_mask(ctx, layer, &geometry) {
        src = crate::yuv::mask(e, &src, &mask)?;
    }
    let image = prepare_resident(e, r, ctx, layer, src, footage.scale)?;
    let result = composite_resident(e, r, ctx, layer, canvas, opacity, image.clone());
    if result == Some(Some(()))
        && let Some((key, token)) = cache
    {
        e.keep_native(footage.pixels, key, token, metadata, image);
    }
    result
}

#[allow(clippy::too_many_arguments)]
fn finish_resident(e: &mut Enc, r: &Renderer, ctx: &EvalCtx, layer: &Layer, canvas: &mut GpuImage, opacity: f32, src: GpuImage, bs: f64) -> Option<Option<()>> {
    let image = prepare_resident(e, r, ctx, layer, src, bs)?;
    composite_resident(e, r, ctx, layer, canvas, opacity, image)
}

fn prepare_resident(e: &mut Enc, r: &Renderer, ctx: &EvalCtx, layer: &Layer, src: GpuImage, bs: f64) -> Option<crate::context::NativeImage> {
    // Native YUV entries retain their historical blending-space representation.
    let mut image = prepare_processed(e, r, ctx, layer, src, bs)?;
    if let Some(c) = r.pipe().to_blend() {
        image.img = ops::convert(e, &image.img, &c);
    }
    Some(image)
}

/// Processed slots also serve lazy CPU consumers, so they must precede to_blend.
fn prepare_processed(e: &mut Enc, r: &Renderer, ctx: &EvalCtx, layer: &Layer, mut src: GpuImage, bs: f64) -> Option<crate::context::NativeImage> {
    let pipe = r.pipe();
    // `masked_source` quantises a non-solid source to the project's depth.
    if let Some(l) = pipe.levels {
        src = ops::quantize(e, &src, l);
    }
    let (img, offset, scale) = if layer.switches.effects && layer.effects().is_some_and(|f| !f.children.is_empty()) {
        // Effects run in effect space (0.7.0): from the layer bounds' corner.
        let o = ctx.effect_bounds(layer).1;
        let mut fx = crate::effects::GpuFx::new(e, src, [o[0] * bs, o[1] * bs], bs, pipe.levels);
        r.run_layer_effects_on(ctx, layer, &mut fx);
        let (img, off, sc) = fx.finish()?;
        (img, [off[0] - o[0] * sc, off[1] - o[1] * sc], sc)
    } else {
        (src, [0.0; 2], bs)
    };
    Some(crate::context::NativeImage { img, offset, scale })
}

#[allow(clippy::too_many_arguments)]
fn composite_resident(
    e: &mut Enc,
    r: &Renderer,
    ctx: &EvalCtx,
    layer: &Layer,
    canvas: &mut GpuImage,
    opacity: f32,
    image: crate::context::NativeImage,
) -> Option<Option<()>> {
    let crate::context::NativeImage { img, offset, scale } = image;
    if img.width == 0 || img.height == 0 {
        return Some(Some(()));
    }
    let pipe = r.pipe();
    let shape = Buf { img: Image { width: img.width, height: img.height, data: Vec::new() }, offset, scale };
    if r.track_matte(ctx, layer).is_some() || layer.preserve_transparency {
        let Some(iso) = place_src(e, r, ctx, layer, &shape, img, &e.zeros(canvas.width, canvas.height), BlendMode::Normal, 1.0, None) else {
            return Some(None);
        };
        return Some(composite_iso(e, r, ctx, layer, iso, canvas, opacity));
    }
    Some(place_src(e, r, ctx, layer, &shape, img, canvas, layer.blend_mode, opacity, pipe.levels).map(|c| *canvas = c))
}

/// Adjustment layer (`Renderer::draw_adjustment`): the effect stack runs on the comp below,
/// resident on the GPU (non-GPU effects read back, run on the CPU and upload), and the result
/// replaces the canvas inside the layer's footprint × opacity. `None` = draw it on the CPU.
fn draw_adjustment(e: &mut Enc, r: &Renderer, ctx: &EvalCtx, layer: &Layer, canvas: &mut GpuImage) -> Option<()> {
    // A track matte or a blending mode on the adjustment layer is applied by the CPU path.
    if r.track_matte(ctx, layer).is_some() || layer.blend_mode != BlendMode::Normal {
        return None;
    }
    let opacity = r.layer_opacity(ctx, layer);
    let Some(foot) = r.adjustment_footprint(ctx, layer) else { return Some(()) };
    let (w, h) = (canvas.width, canvas.height);
    let matte = place(e, r, ctx, layer, &foot, &e.zeros(w, h), BlendMode::Normal, 1.0, None)?;
    let pipe = r.pipe();
    // Effects run in the working space, not the linear blending space.
    let below = match pipe.from_blend() {
        Some(c) => ops::convert(e, canvas, &c),
        None => canvas.clone(),
    };
    let mut fx = crate::effects::GpuFx::new(e, below, [0.0; 2], r.opts.scale, pipe.levels);
    r.run_effects_on(ctx, layer, true, &mut fx);
    let (mut adjusted, offset, _) = fx.finish()?;
    if adjusted.width != w || adjusted.height != h {
        // An effect grew the buffer (Motion Tile's Output Width): back to the comp.
        let Some((x, y)) = effectcraft_render::adjustment_crop(offset, [0.0; 2], (adjusted.width, adjusted.height), (w, h)) else {
            return Some(());
        };
        adjusted = e.crop(&adjusted, x as u32, y as u32, w, h);
    }
    if let Some(c) = pipe.to_blend() {
        adjusted = ops::convert(e, &adjusted, &c);
    }
    *canvas = ops::adjust_mix(e, canvas, &adjusted, &matte, opacity);
    Some(())
}

fn composite_layer(e: &mut Enc, r: &Renderer, ctx: &EvalCtx, layer: &Layer, buf: &Arc<Buf>, canvas: &mut GpuImage, opacity: f32) -> Option<()> {
    let matte = r.track_matte(ctx, layer).is_some();
    if !matte && !layer.preserve_transparency {
        // The comp is quantised after the layer (8/16 bpc) in the same pass.
        *canvas = place(e, r, ctx, layer, buf, canvas, layer.blend_mode, opacity, r.pipe().levels)?;
        return Some(());
    }
    // Render in isolation, then matte / preserve transparency, then blend.
    let iso = place(e, r, ctx, layer, buf, &e.zeros(canvas.width, canvas.height), BlendMode::Normal, 1.0, None)?;
    composite_iso(e, r, ctx, layer, iso, canvas, opacity)
}

/// Track matte / Preserve Transparency on an isolated layer render, then blend.
fn composite_iso(e: &mut Enc, r: &Renderer, ctx: &EvalCtx, layer: &Layer, iso: GpuImage, canvas: &mut GpuImage, opacity: f32) -> Option<()> {
    composite_iso_with(e, r, ctx, layer, iso, canvas, opacity, None, r.pipe().levels)
}

/// `Renderer::composite_iso_with`: the matte from `matte_img` when given (a 3D matte drawn
/// through the camera), else the matte layer placed in 2D; the blend quantised to `levels`.
#[allow(clippy::too_many_arguments)]
fn composite_iso_with(
    e: &mut Enc,
    r: &Renderer,
    ctx: &EvalCtx,
    layer: &Layer,
    mut iso: GpuImage,
    canvas: &mut GpuImage,
    opacity: f32,
    matte_img: Option<GpuImage>,
    levels: Option<f32>,
) -> Option<()> {
    if let Some((m, kind)) = r.track_matte(ctx, layer) {
        let mimg = match matte_img.filter(|i| (i.width, i.height) == (canvas.width, canvas.height)) {
            Some(i) => i,
            None => {
                let mut mimg = e.zeros(canvas.width, canvas.height);
                if m.is_active_at(ctx.time)
                    && let Some(mb) = r.matte_buf(ctx, m)
                {
                    let mo = ctx.opacity(m) as f32;
                    mimg = place(e, r, ctx, m, &mb, &mimg, BlendMode::Normal, mo, None)?;
                }
                mimg
            }
        };
        iso = ops::matte(e, &iso, &mimg, kind);
    }
    *canvas = ops::blend_full_preserve(e, canvas, &iso, layer.blend_mode, opacity, layer.id.0 as u32, levels, layer.preserve_transparency);
    Some(())
}

/// Draw a processed layer buffer onto `target` (transform, motion blur sub-samples), quantising
/// the result to `levels` when set.
#[allow(clippy::too_many_arguments)]
fn place(
    e: &mut Enc,
    r: &Renderer,
    ctx: &EvalCtx,
    layer: &Layer,
    buf: &Arc<Buf>,
    target: &GpuImage,
    mode: BlendMode,
    opacity: f32,
    levels: Option<f32>,
) -> Option<GpuImage> {
    if buf.img.is_empty() || opacity <= 0.0 {
        return Some(target.clone());
    }
    let src = e.upload_buf(buf)?;
    place_src(e, r, ctx, layer, buf, src, target, mode, opacity, levels)
}

/// [`place`] of a GPU image `src` shaped like `buf`.
#[allow(clippy::too_many_arguments)]
fn place_src(
    e: &mut Enc,
    r: &Renderer,
    ctx: &EvalCtx,
    layer: &Layer,
    buf: &Buf,
    src: GpuImage,
    target: &GpuImage,
    mode: BlendMode,
    opacity: f32,
    levels: Option<f32>,
) -> Option<GpuImage> {
    let pl = r.placement(ctx, layer, buf);
    if let [m] = pl.matrices.as_slice() {
        return Some(ops::warp_q(e, target, &src, m, pl.sampling, mode, opacity, pl.seed, None, levels));
    }
    let k = 1.0 / pl.matrices.len() as f32;
    let acc = ops::warp_accumulate(e, &e.zeros(target.width, target.height), &src, &pl.matrices, pl.sampling, k);
    Some(ops::blend_full(e, target, &acc, mode, opacity, pl.seed, levels))
}
