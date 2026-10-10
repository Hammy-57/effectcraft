//! The frame pipeline shared by every output format: Render Settings overrides (effects, solo,
//! guide layers, frame blending, motion blur), field rendering with 3:2 pulldown, colour depth,
//! the Output Module's crop and resize, alpha handling, storage overflow and the render log.

use std::sync::Mutex;

use effectcraft_project::render_queue::{
    AlphaMode, Channels, ColorDepth, CurrentOrOff, EffectsMode, FrameSample, RenderLog, ResizeQuality, SwitchOverride, log_path,
};
use effectcraft_project::{Comp, Project};
use effectcraft_raster::Image;
use effectcraft_render::{Backend, LayerCache, RenderOpts, Renderer};
use effectcraft_time::Tick;
use web_time::Instant;

use crate::{Job, RenderQuality, out};

/// A job being exported: the job, the project with the Render Settings overrides applied, the
/// job's layer cache and per-frame timing for the render log.
pub(crate) struct Cx<'a> {
    pub job: &'a Job<'a>,
    pub project: Project,
    /// Processed layer buffers shared by the job's frames, as the viewer's layer cache: static
    /// and transform-only layers render once per job (the GPU compositor then uploads them
    /// once). Dropped with the job.
    cache: LayerCache,
    /// (output frame, seconds) of every rendered frame (Plus Per Frame Info logs).
    pub frame_times: Option<Mutex<Vec<(u64, f64)>>>,
    /// Files written to an overflow folder.
    pub overflowed: Mutex<Vec<String>>,
    written: Mutex<Vec<String>>,
}

impl<'a> std::ops::Deref for Cx<'a> {
    type Target = Job<'a>;
    fn deref(&self) -> &Job<'a> {
        self.job
    }
}

/// The project as the Render Settings render it: Effects All On / All Off, Solo Switches All
/// Off, Frame Blending and Motion Blur "On for Checked Layers" (the composition switches on).
pub(crate) fn apply_overrides(p: &mut Project, job: &Job) {
    let s = job.settings;
    let ids: Vec<_> = p.comps().map(|(i, _)| *i).collect();
    for id in ids {
        let Some(c) = p.comp_mut(id) else { continue };
        match s.frame_blending {
            SwitchOverride::Current => {}
            SwitchOverride::OnForChecked => c.enable_frame_blending = true,
            SwitchOverride::OffForAll => c.enable_frame_blending = false,
        }
        match s.motion_blur_override() {
            SwitchOverride::Current => {}
            SwitchOverride::OnForChecked => c.enable_motion_blur = true,
            SwitchOverride::OffForAll => c.enable_motion_blur = false,
        }
        for l in c.layers.iter_mut() {
            match s.effects {
                EffectsMode::Current => {}
                EffectsMode::AllOn => l.switches.effects = true,
                EffectsMode::AllOff => l.switches.effects = false,
            }
            if s.solo == CurrentOrOff::AllOff {
                l.switches.solo = false;
            }
        }
    }
}

impl<'a> Cx<'a> {
    pub fn new(job: &'a Job<'a>) -> Cx<'a> {
        let mut project = job.project.clone();
        apply_overrides(&mut project, job);
        let frame_times = (job.options.log == RenderLog::PlusPerFrameInfo).then(|| Mutex::new(Vec::new()));
        let cache = LayerCache::default();
        // With deferred readbacks, buffers computed from a missed pass's placeholders are not kept.
        cache.set_gate(job.accel.and_then(|a| a.miss_gate()));
        Cx { job, project, cache, frame_times, overflowed: Mutex::new(vec![]), written: Mutex::new(vec![]) }
    }

    pub fn comp(&self) -> Option<&Comp> {
        self.project.comp(self.job.comp)
    }

    /// The renderer of the job's frames: the Render Settings' options, the job's accelerator
    /// and layer cache.
    fn renderer(&self) -> Renderer<'_> {
        let s = self.job.settings;
        // The whole job renders on the project's renderer (Mercury GPU Acceleration or Software
        // Only), not on whichever compositor Auto timed faster for each frame: the CPU and GPU
        // compositors differ slightly, so a per-frame choice made renders differ run to run.
        let backend = if self.project.settings.gpu_acceleration { Backend::Gpu } else { Backend::Cpu };
        let opts = RenderOpts {
            scale: s.resolution.clamp(0.01, 4.0),
            motion_blur: s.motion_blur_override() != SwitchOverride::OffForAll,
            guides: s.guide_layers == CurrentOrOff::Current,
            draft: s.quality == RenderQuality::Draft,
            // Output renders always look through the comp's active camera.
            view: None,
            backend,
            roi: None,
            nested_switches: self.job.nested_switches,
            draft_shadows: true,
            proxy: s.proxy_use,
        };
        let mut r = Renderer::new(&self.project, self.job.footage, opts);
        r.expr = self.job.expr;
        r.accel = self.job.accel;
        r.cache = Some(&self.cache);
        r
    }

    async fn render_at(&self, t: Tick) -> Image {
        // With deferred GPU readbacks (a browser worker) the frame renders in passes.
        effectcraft_render::passes::comp_frame(&self.renderer(), self.job.comp, t).await
    }

    /// Whether frames render in passes (an accelerator with deferred readbacks).
    pub fn deferred(&self) -> bool {
        effectcraft_render::passes::is_deferred(self.job.accel)
    }

    /// Output frames `ks`, each mapped by `f` (encoding): rendered in parallel batches, or one
    /// by one in passes when readbacks are deferred ([`Cx::deferred`]).
    pub async fn frames<T: Send>(&self, comp: &Comp, ks: Vec<u64>, f: impl Fn(u64, Image) -> T + Sync + Send) -> Vec<T> {
        if self.deferred() {
            let mut out = Vec::with_capacity(ks.len());
            for k in ks {
                let img = self.frame_async(comp, k).await;
                out.push(f(k, img));
            }
            return out;
        }
        use rayon::prelude::*;
        ks.into_par_iter().map(|k| f(k, self.frame(comp, k))).collect()
    }

    /// Output frames `0..total` in batches of `batch` frames, each frame mapped by `f`
    /// (encoding), handed to `body` in order through [`Batches::next`]. On native targets the
    /// next batch renders on a scoped thread while `body` encodes the current one: at most two
    /// batches are held, the one being encoded and the one rendered next (handed over through a
    /// rendezvous channel). In passes (deferred readbacks), on the web and on a rayon worker (it
    /// must not block on frames its own pool renders) batches render one after another.
    ///
    /// When `body` returns (done, an encode error or a stop) the renderer stops at its next
    /// frame; a renderer that stops early (a panic) makes [`Batches::next`] an error.
    pub async fn batches<T: Send, R>(
        &self,
        comp: &Comp,
        total: u64,
        batch: u64,
        f: impl Fn(u64, Image) -> T + Sync + Send,
        body: impl AsyncFnOnce(&mut Batches<'_, '_, T>) -> crate::Result<R>,
    ) -> crate::Result<R> {
        let batch = batch.max(1);
        if self.deferred() || cfg!(target_arch = "wasm32") || rayon::current_thread_index().is_some() {
            let f: &(dyn Fn(u64, Image) -> T + Sync + Send) = &f;
            return body(&mut Batches { next: 0, total, batch, src: Source::Serial { cx: self, comp, f } }).await;
        }
        use rayon::prelude::*;
        use std::sync::atomic::{AtomicBool, Ordering};
        let stop = AtomicBool::new(false);
        std::thread::scope(|s| {
            let (tx, rx) = std::sync::mpsc::sync_channel::<Vec<T>>(0);
            let (stop, f) = (&stop, &f);
            let renderer = s.spawn(move || {
                let mut i = 0;
                while i < total && !stop.load(Ordering::Relaxed) {
                    let end = i.saturating_add(batch).min(total);
                    // Frames not started once the encode side has stopped are skipped.
                    let frames: Option<Vec<T>> = (i..end).into_par_iter().map(|k| (!stop.load(Ordering::Relaxed)).then(|| f(k, self.frame(comp, k)))).collect();
                    let Some(frames) = frames else { break };
                    if tx.send(frames).is_err() {
                        break;
                    }
                    i = end;
                }
            });
            let mut batches = Batches { next: 0, total, batch, src: Source::Thread(rx) };
            let r = effectcraft_render::passes::block_on(body(&mut batches));
            // Stop the renderer: skip its remaining frames and fail its next hand-off.
            stop.store(true, Ordering::Relaxed);
            drop(batches);
            match (r, renderer.join()) {
                (Err(_), Err(_)) => Err(crate::ExportError::Encode("rendering failed".into())),
                (r, _) => r,
            }
        })
    }

    /// Output frame `i`: sampled (fields, pulldown), at the colour depth, cropped and resized.
    pub fn frame(&self, comp: &Comp, i: u64) -> Image {
        effectcraft_render::passes::block_on(self.frame_async(comp, i))
    }

    /// [`Cx::frame`] as a future (deferred readbacks: the frame renders in passes).
    pub async fn frame_async(&self, comp: &Comp, i: u64) -> Image {
        let t0 = Instant::now();
        let img = match self.job.settings.sample(comp, i) {
            FrameSample::Progressive(t) => self.render_at(t).await,
            FrameSample::Fields { first, second, upper_first } => {
                let a = self.render_at(first).await;
                if second == first { a } else { interleave(&a, &self.render_at(second).await, upper_first) }
            }
        };
        let mut img = img;
        // Color Depth: renders run at the project's depth; another depth quantises the frame.
        let depth = self.job.settings.color_depth;
        if depth != ColorDepth::Current
            && let Some(levels) = depth.resolve(self.project.settings.bit_depth).levels()
        {
            img.quantize(levels);
        }
        let img = self.crop_resize(img);
        if let Some(ft) = &self.frame_times {
            ft.lock().unwrap_or_else(|e| e.into_inner()).push((i, t0.elapsed().as_secs_f64()));
        }
        img
    }

    /// Output Module ▸ Crop, then Resize.
    fn crop_resize(&self, img: Image) -> Image {
        let om = self.job.output;
        let scale = self.job.settings.resolution.clamp(0.01, 4.0);
        let (t, l, b, r) = om.crop.edges(img.width, img.height, scale);
        let img = if (t, l, b, r) == (0, 0, 0, 0) { img } else { crop(&img, t, l, b, r) };
        if !om.resize.enabled {
            return img;
        }
        let (w, h) = om.frame_size(img.width, img.height, 1.0);
        if (w, h) == (img.width, img.height) {
            return img;
        }
        match om.resize.quality {
            ResizeQuality::Low => effectcraft_raster::warp::resample(&img, w, h),
            ResizeQuality::High => bicubic(&img, w, h),
        }
    }

    /// 8-bit RGBA of the requested channels cropped/padded to `w`×`h` (codec rounding): RGB over
    /// the comp background (opaque), RGB + Alpha straight or premultiplied, or the alpha matte.
    pub fn pixels(&self, img: &Image, comp: &Comp, channels: Channels, w: u32, h: u32) -> Vec<u8> {
        let px = match channels {
            Channels::Rgb => img.to_rgba8_over(comp.background),
            Channels::Rgba => match self.job.output.alpha_mode {
                AlphaMode::Straight => img.to_rgba8(),
                AlphaMode::Premultiplied => premultiplied8(img),
            },
            Channels::Alpha => img
                .data
                .iter()
                .flat_map(|p| {
                    let a = (p[3].clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
                    [a, a, a, 255]
                })
                .collect(),
        };
        crate::fit(px, img.width, img.height, w, h)
    }

    /// Open a file and remember ownership for cancellation cleanup (including overflow paths).
    pub fn create(&self, path: &str) -> crate::Result<out::Out<'_>> {
        let file = out::create(self.sink, path)?;
        if self.sink.is_none() {
            self.written.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(path.to_owned());
        }
        Ok(file)
    }

    pub fn remove_cancelled_outputs(&self) -> crate::Result<()> {
        let mut failure = None;
        for path in self.written.lock().unwrap_or_else(std::sync::PoisonError::into_inner).iter() {
            if let Err(e) = std::fs::remove_file(path)
                && e.kind() != std::io::ErrorKind::NotFound
            {
                failure.get_or_insert_with(|| crate::ExportError::Io(format!("cannot remove cancelled output {path}: {e}")));
            }
        }
        match failure {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    /// Where a file goes: `path`, or (Use Storage Overflow) the first overflow folder with room
    /// for `bytes` when the quota hook says the output volume is full.
    pub fn place(&self, path: &str, bytes: u64) -> String {
        let Some(q) = self.job.options.storage else { return path.to_string() };
        if !self.job.settings.storage_overflow || q.has_room(path, bytes) {
            return path.to_string();
        }
        let name = std::path::Path::new(path).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| path.to_string());
        for dir in &self.job.options.overflow {
            let p = std::path::Path::new(dir).join(&name).to_string_lossy().to_string();
            if q.has_room(&p, bytes) {
                self.overflowed.lock().unwrap_or_else(|e| e.into_inner()).push(p.clone());
                return p;
            }
        }
        path.to_string()
    }

    /// Write the render log next to the output, as the Log setting asks. Returns its path.
    pub fn write_log(&self, result: &Result<crate::Report, crate::ExportError>, seconds: f64) -> Option<String> {
        let level = self.job.options.log;
        // Errors Only: a log for failures (a user stop is not an error).
        if level == RenderLog::ErrorsOnly && matches!(result, Ok(_) | Err(crate::ExportError::Cancelled)) {
            return None;
        }
        let comp = self.comp();
        let path = log_path(self.job.path);
        let mut s = String::new();
        s.push_str("EffectCraft Render Log\n");
        s.push_str(&format!("Item: {}\n", if self.job.options.label.is_empty() { "(render)" } else { &self.job.options.label }));
        s.push_str(&format!("Output: {}\n", self.job.path));
        match result {
            Ok(r) => s.push_str(&format!("Result: Done ({} frames, {}×{}, {} bytes) in {seconds:.2} s\n", r.frames, r.width, r.height, r.bytes)),
            Err(e) => s.push_str(&format!("Result: Failed\nError: {e}\n")),
        }
        let over = self.overflowed.lock().unwrap_or_else(|e| e.into_inner());
        if !over.is_empty() {
            s.push_str(&format!("Storage overflow: {} file(s) written to overflow folders (first: {})\n", over.len(), over[0]));
        }
        if level != RenderLog::ErrorsOnly {
            s.push_str("\nRender Settings:\n");
            for (k, v) in self.job.settings.describe(comp) {
                s.push_str(&format!("  {k}: {v}\n"));
            }
            s.push_str("\nOutput Module:\n");
            for (k, v) in self.job.output.describe() {
                s.push_str(&format!("  {k}: {v}\n"));
            }
        }
        if let (RenderLog::PlusPerFrameInfo, Some(ft), Some(c)) = (level, &self.frame_times, comp) {
            let mut v = ft.lock().unwrap_or_else(|e| e.into_inner()).clone();
            v.sort_by_key(|(i, _)| *i);
            s.push_str("\nFrames:\n");
            let f0 = self.job.settings.first_frame(c);
            for (i, secs) in v {
                let t = self.job.settings.frame_time(c, i);
                s.push_str(&format!("  Frame {} ({:.3} s): rendered in {:.1} ms\n", f0 + i as i64, t.seconds(), secs * 1000.0));
            }
        }
        let mut f = self.create(&path).ok()?;
        std::io::Write::write_all(&mut f, s.as_bytes()).ok()?;
        f.finish().ok()?;
        Some(path)
    }
}

/// The batches of [`Cx::batches`], in order.
pub(crate) struct Batches<'c, 'a, T> {
    next: u64,
    total: u64,
    batch: u64,
    src: Source<'c, 'a, T>,
}

enum Source<'c, 'a, T> {
    /// Rendered when asked for ([`Cx::frames`]).
    Serial { cx: &'c Cx<'a>, comp: &'c Comp, f: &'c (dyn Fn(u64, Image) -> T + Sync + Send) },
    /// Rendered ahead on the renderer thread.
    Thread(std::sync::mpsc::Receiver<Vec<T>>),
}

impl<T: Send> Batches<'_, '_, T> {
    /// The next batch: its output frames `start..end` and their mapped frames; `None` after the last.
    pub async fn next(&mut self) -> crate::Result<Option<(u64, u64, Vec<T>)>> {
        if self.next >= self.total {
            return Ok(None);
        }
        let (i, end) = (self.next, self.next.saturating_add(self.batch).min(self.total));
        let frames = match &self.src {
            Source::Serial { cx, comp, f } => cx.frames(comp, (i..end).collect(), *f).await,
            Source::Thread(rx) => rx.recv().map_err(|_| crate::ExportError::Encode("rendering stopped".into()))?,
        };
        self.next = end;
        Ok(Some((i, end, frames)))
    }
}

/// Weave two fields: the dominant field's lines (even lines when `upper_first`) from `first`.
pub(crate) fn interleave(first: &Image, second: &Image, upper_first: bool) -> Image {
    let mut out = first.clone();
    let w = out.width as usize;
    for y in 0..out.height as usize {
        let from_first = (y % 2 == 0) == upper_first;
        if !from_first && y < second.height as usize && second.width as usize == w {
            out.data[y * w..(y + 1) * w].copy_from_slice(&second.data[y * w..(y + 1) * w]);
        }
    }
    out
}

/// Remove `t`/`l`/`b`/`r` pixels from the edges (negative values add transparent pixels).
pub(crate) fn crop(img: &Image, t: i32, l: i32, b: i32, r: i32) -> Image {
    let w = (img.width as i32 - l - r).max(1) as u32;
    let h = (img.height as i32 - t - b).max(1) as u32;
    let mut out = Image::new(w, h);
    for y in 0..h as i32 {
        let sy = y + t;
        if sy < 0 || sy >= img.height as i32 {
            continue;
        }
        for x in 0..w as i32 {
            let sx = x + l;
            if sx >= 0 && sx < img.width as i32 {
                let o = out.idx(x as u32, y as u32);
                out.data[o] = img.data[img.idx(sx as u32, sy as u32)];
            }
        }
    }
    out
}

/// Bicubic resize (Resize Quality: High).
fn bicubic(src: &Image, w: u32, h: u32) -> Image {
    // Shrinking a lot: bilinear with its box pre-filter first, then bicubic for the last step.
    if src.width > w * 2 && src.height > h * 2 {
        let mid = effectcraft_raster::warp::resample(src, w * 2, h * 2);
        return bicubic(&mid, w, h);
    }
    let mut out = Image::new(w, h);
    let (sx, sy) = (src.width as f64 / w as f64, src.height as f64 / h as f64);
    for y in 0..h {
        for x in 0..w {
            let o = out.idx(x, y);
            // Clamp to the edge so borders stay opaque.
            let fx = ((x as f64 + 0.5) * sx).clamp(0.5, src.width as f64 - 0.5);
            let fy = ((y as f64 + 0.5) * sy).clamp(0.5, src.height as f64 - 0.5);
            let mut p = src.sample_bicubic(fx, fy);
            p[3] = p[3].min(1.0);
            out.data[o] = p;
        }
    }
    out
}

/// Premultiplied 8-bit RGBA (colour matted with black).
fn premultiplied8(img: &Image) -> Vec<u8> {
    img.data
        .iter()
        .flat_map(|p| {
            let a = p[3].clamp(0.0, 1.0);
            [0, 1, 2].map(|c| (p[c].clamp(0.0, a) * 255.0 + 0.5) as u8).into_iter().chain([(a * 255.0 + 0.5) as u8])
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layers_render_once_per_job() {
        // #413: a job rendered every layer of every frame from scratch (no layer cache).
        use effectcraft_color::Label;
        use effectcraft_keyframe::{Keyframe, Value};
        use effectcraft_project::render_queue::{OutputFormat, OutputModule, RenderSettings};
        use effectcraft_project::{ItemKind, LayerSource, Solid, build};
        use effectcraft_render::NoFootage;
        use effectcraft_time::FrameRate;
        // A solid animated only in Transform: its pixels render once, then move.
        let mut p = Project::default();
        let mut comp = Comp::new(64, 48, FrameRate::FPS_25, Tick::from_seconds_f64(1.0));
        let sid = p.add_item("Solid", Label::Red, None, ItemKind::Solid(Solid { color: [1.0, 0.0, 0.0], width: 16, height: 16, pixel_aspect: 1.0 }));
        let mut l = build::layer(&mut p, &comp, "Solid", LayerSource::Solid { item: sid }, (16, 16), None);
        l.props.prop_mut("transform/position").unwrap().keys =
            vec![Keyframe::new(Tick::ZERO, Value::Vec3([8.0, 24.0, 0.0])), Keyframe::new(Tick::from_seconds_f64(1.0), Value::Vec3([56.0, 24.0, 0.0]))];
        comp.layers.push(l);
        let cid = p.add_item("Comp", Label::Sandstone, None, ItemKind::Comp(comp.into()));
        let (settings, output) = (RenderSettings::default(), OutputModule::for_format(OutputFormat::PngSequence));
        let job = Job {
            project: &p,
            footage: &NoFootage,
            expr: None,
            accel: None,
            comp: cid,
            settings: &settings,
            output: &output,
            path: "/c/f_[#####].png",
            sink: None,
            nested_switches: true,
            options: Default::default(),
        };
        let cx = Cx::new(&job);
        let comp = cx.comp().unwrap();
        cx.frame(comp, 0);
        let renders = cx.cache.stats().misses;
        assert!(renders > 0);
        for i in 1..10 {
            let img = cx.frame(comp, i);
            // The frames are the ones rendered without a cache.
            let t = settings.frame_time(comp, i);
            let mut uncached = cx.renderer();
            uncached.cache = None;
            assert_eq!(img.data, uncached.comp_frame(cid, t).data, "frame {i}");
        }
        let stats = cx.cache.stats();
        assert_eq!(stats.misses, renders, "the solid rendered on the first frame only");
        assert!(stats.hits >= 9, "{stats:?}");
    }

    /// Run `f` with the job of a `frames`-frame comp of one solid.
    fn with_cx<R>(frames: u64, f: impl FnOnce(&Cx, &Comp) -> R) -> R {
        use effectcraft_color::Label;
        use effectcraft_project::render_queue::{OutputFormat, OutputModule, RenderSettings};
        use effectcraft_project::{ItemKind, LayerSource, Solid, build};
        use effectcraft_render::NoFootage;
        use effectcraft_time::FrameRate;
        let mut p = Project::default();
        let mut comp = Comp::new(16, 16, FrameRate::FPS_25, FrameRate::FPS_25.tick_of(frames as i64));
        let sid = p.add_item("Solid", Label::Red, None, ItemKind::Solid(Solid { color: [1.0, 0.0, 0.0], width: 8, height: 8, pixel_aspect: 1.0 }));
        comp.layers.push(build::layer(&mut p, &comp, "Solid", LayerSource::Solid { item: sid }, (8, 8), None));
        let cid = p.add_item("Comp", Label::Sandstone, None, ItemKind::Comp(comp.into()));
        let (settings, output) = (RenderSettings::default(), OutputModule::for_format(OutputFormat::H264));
        let job = Job {
            project: &p,
            footage: &NoFootage,
            expr: None,
            accel: None,
            comp: cid,
            settings: &settings,
            output: &output,
            path: "/c/x.mp4",
            sink: None,
            nested_switches: true,
            options: Default::default(),
        };
        let cx = Cx::new(&job);
        let comp = cx.comp().unwrap();
        f(&cx, comp)
    }

    #[test]
    fn an_encode_error_stops_the_renderer() {
        // G5 B1: the next batch renders while one encodes. An encode error returns at once;
        // the renderer stops after at most the batch it had in flight (no hang on the hand-off).
        use std::sync::atomic::{AtomicU64, Ordering};
        let batch = 4;
        let rendered = AtomicU64::new(0);
        let r = with_cx(batch * 10, |cx, comp| {
            let render = |k: u64, _| {
                rendered.fetch_add(1, Ordering::Relaxed);
                k
            };
            effectcraft_render::passes::block_on(cx.batches(comp, batch * 10, batch, render, async |b: &mut Batches<'_, '_, u64>| -> crate::Result<()> {
                let (i, end, ks) = b.next().await?.unwrap();
                assert_eq!((i, end, ks), (0, batch, vec![0, 1, 2, 3]));
                Err(crate::ExportError::Encode("boom".into()))
            }))
        });
        assert!(matches!(r, Err(crate::ExportError::Encode(ref e)) if e == "boom"), "{r:?}");
        let n = rendered.load(Ordering::Relaxed);
        assert!(n <= batch * 2, "rendered {n} frames after the encoder failed");
    }

    #[test]
    fn batches_arrive_in_order_and_a_failed_render_is_an_error() {
        let batch = 3;
        let got = with_cx(10, |cx, comp| {
            effectcraft_render::passes::block_on(cx.batches(
                comp,
                10,
                batch,
                |k, _| k,
                async |b: &mut Batches<'_, '_, u64>| -> crate::Result<Vec<(u64, u64, Vec<u64>)>> {
                    let mut v = vec![];
                    while let Some(x) = b.next().await? {
                        v.push(x);
                    }
                    Ok(v)
                },
            ))
        });
        assert_eq!(got.unwrap(), vec![(0, 3, vec![0, 1, 2]), (3, 6, vec![3, 4, 5]), (6, 9, vec![6, 7, 8]), (9, 10, vec![9])]);
        // A renderer that dies (a panic in the second batch) fails the export instead of
        // ending it early with a truncated file, and does not hang.
        let r = with_cx(10, |cx, comp| {
            let render = |k: u64, _| {
                assert!(k != 4, "render failure");
                k
            };
            effectcraft_render::passes::block_on(cx.batches(comp, 10, batch, render, async |b: &mut Batches<'_, '_, u64>| -> crate::Result<u64> {
                let mut n = 0;
                while let Some((_, _, ks)) = b.next().await? {
                    n += ks.len() as u64;
                }
                Ok(n)
            }))
        });
        assert!(matches!(r, Err(crate::ExportError::Encode(_))), "{r:?}");
    }

    #[test]
    fn interleave_takes_alternate_lines() {
        let a = Image::filled(3, 4, [1.0, 0.0, 0.0, 1.0]);
        let b = Image::filled(3, 4, [0.0, 0.0, 1.0, 1.0]);
        let up = interleave(&a, &b, true);
        let lo = interleave(&a, &b, false);
        for y in 0..4u32 {
            assert_eq!(up.data[up.idx(1, y)][0], if y % 2 == 0 { 1.0 } else { 0.0 }, "upper first: line {y}");
            assert_eq!(lo.data[lo.idx(1, y)][0], if y % 2 == 1 { 1.0 } else { 0.0 }, "lower first: line {y}");
        }
    }

    #[test]
    fn crop_and_pad() {
        let mut img = Image::new(4, 3);
        for (i, p) in img.data.iter_mut().enumerate() {
            *p = [i as f32, 0.0, 0.0, 1.0];
        }
        let c = crop(&img, 1, 1, 0, 1);
        assert_eq!((c.width, c.height), (2, 2));
        assert_eq!(c.data[0][0], 5.0);
        let p = crop(&img, -1, 0, 0, 0);
        assert_eq!((p.width, p.height), (4, 4));
        assert_eq!(p.data[0], [0.0; 4]);
        assert_eq!(p.data[4][0], 0.0);
        assert_eq!(p.data[5][0], 1.0);
    }
}
