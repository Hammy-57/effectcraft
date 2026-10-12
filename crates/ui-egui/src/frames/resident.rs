//! RT10e: viewer frames shown straight from their GPU textures (native).
//!
//! When the GPU compositor shares egui's wgpu device (the desktop app, unless switched off in
//! Settings ▸ Previews or with `EFFECTCRAFT_ZERO_COPY=0`), a frame the GPU composited stays a
//! texture: egui draws it as it is ([`super::FrameImage::Gpu`]), with no readback to the CPU and
//! no upload back to the GPU. Frames for the disk cache and for CPU consumers (Info panel
//! sampling, eyedroppers, histograms, export) are still read back, when they are needed.
//!
//! One queue serves the compositor and the window then, and egui's drawing waits behind the
//! compositor work submitted before it. So a frame worker doesn't run ahead of the GPU: past
//! [`MAX_IN_FLIGHT`] frames whose GPU work hasn't finished, it waits for the oldest (bounded by
//! the readback deadline, whose expiry retires the compositor: the frame then renders on the CPU).

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use effectcraft_gpu::Gpu;
use effectcraft_gpu::wgpu::SubmissionIndex;
use web_time::Instant;

/// Resident frames whose GPU work may be unfinished at once.
pub(super) const MAX_IN_FLIGHT: usize = 2;

#[derive(Default)]
pub(super) struct Resident {
    on: AtomicBool,
    /// Fences of the resident frames submitted last, oldest first.
    flights: Mutex<VecDeque<(Gpu, SubmissionIndex)>>,
    /// When the GPU finished the last resident frame (its next frame's GPU time starts no earlier).
    last_done: Arc<Mutex<Option<Instant>>>,
    /// Frames handed to the viewer as textures (diagnostics, tests).
    pub(super) shown: AtomicU64,
}

impl Resident {
    pub(super) fn set(&self, on: bool) {
        if !self.on.swap(on, Ordering::AcqRel) || on {
            return;
        }
        self.flights.lock().unwrap_or_else(PoisonError::into_inner).clear();
    }

    pub(super) fn on(&self) -> bool {
        self.on.load(Ordering::Acquire)
    }

    /// A resident frame's GPU work was submitted (encoding started at `t0`): fence it, record
    /// Auto's GPU time for it once the GPU is done (`auto`), and wait for the oldest frames past
    /// [`MAX_IN_FLIGHT`]. `Err` when that wait failed (the compositor retired).
    pub(super) fn admit(&self, gpu: &Gpu, auto: Option<effectcraft_engine::render::AutoKey>, t0: Instant) -> Result<(), String> {
        let submitted = Instant::now();
        let fence = gpu.context().fence();
        self.shown.fetch_add(1, Ordering::Relaxed);
        let (g, last_done) = (gpu.clone(), self.last_done.clone());
        // Completion callbacks run in submission order, from whichever thread polls the device.
        gpu.context().queue.on_submitted_work_done(move || {
            let now = Instant::now();
            let mut last = last_done.lock().unwrap_or_else(PoisonError::into_inner);
            // The frame's own GPU time: from its submission, or from when the GPU finished the
            // frame ahead of it, whichever is later (as the landing thread leaves out the queue).
            let gpu_start = last.map_or(submitted, |l| l.max(submitted));
            *last = Some(now);
            if let (Some(key), Some(p)) = (auto, effectcraft_engine::render::Accelerator::auto_pick(&g)) {
                let ms = (submitted.duration_since(t0) + now.duration_since(gpu_start)).as_secs_f64() * 1000.0;
                p.record(key, true, ms);
            }
        });
        let waits: Vec<_> = {
            let mut f = self.flights.lock().unwrap_or_else(PoisonError::into_inner);
            f.push_back((gpu.clone(), fence));
            let over = f.len().saturating_sub(MAX_IN_FLIGHT);
            f.drain(..over).collect()
        };
        for (g, fence) in waits {
            g.wait_submission(&fence)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::*;
    use super::MAX_IN_FLIGHT;
    use effectcraft_engine::Session;
    use effectcraft_engine::render::{Accelerator, Backend};
    use serde_json::json;

    fn wait(f: &Frames) {
        for _ in 0..6000 {
            if f.inflight() == 0 {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        panic!("frames did not finish");
    }

    /// A comp with straight colour, partial alpha (premultiplied rounding) and empty pixels.
    fn scene() -> (Session, ItemId) {
        let mut s = Session::default();
        let cid = ItemId(s.execute("comp.new", json!({"name": "Z", "width": 160, "height": 90, "duration": 2})).unwrap()["comp"].as_u64().unwrap());
        s.execute("layer.newSolid", json!({"color": "#3080ff", "width": 120, "height": 60})).unwrap();
        let top = s.execute("layer.newSolid", json!({"color": "#ff7a10", "width": 70, "height": 70})).unwrap()["layer"].as_u64().unwrap();
        s.execute("prop.set", json!({"layer": top, "path": "transform/opacity", "value": 37})).unwrap();
        (s, cid)
    }

    fn source(s: &Session, gpu: &Gpu) -> RenderSource {
        RenderSource {
            freeze_feedback: Default::default(),
            project: s.project.clone(),
            footage: Arc::new(effectcraft_engine::render::NoFootage),
            expr: None,
            layer_cache: Arc::new(LayerCache::new(1 << 30)),
            gpu: Some(gpu.clone()),
            gpu_display: true,
            disk: None,
        }
    }

    fn render(f: &Frames, s: &Session, src: &RenderSource, cid: ItemId, frame: i64) -> FrameImage {
        let key = FrameKey { revision: s.revision, content: f.content_of(&s.project, s.revision, cid), comp: cid.0, frame, scale: 1000, view: 0, opts: 0 };
        f.request_urgent(src, key, cid, Tick::from_seconds_f64(frame as f64 / 30.0), RenderOpts { backend: Backend::Gpu, ..Default::default() });
        wait(f);
        f.get(&key).expect("the frame is cached")
    }

    /// RT10e: a frame drawn from its texture is pixel for pixel the frame the readback path shows
    /// (and the blocking readback the workers used to wait for), and it skips the landing thread.
    #[test]
    fn resident_frames_equal_readback_frames_pixel_for_pixel() {
        let Some(gpu) = Gpu::headless() else {
            assert!(std::env::var("EFFECTCRAFT_REQUIRE_GPU").as_deref() != Ok("1"), "GPU required");
            return;
        };
        // Submissions go through a (green) traffic light, as on the window's device.
        let light = effectcraft_gpu::TrafficLight::new();
        assert!(gpu.context().set_traffic_light(light.clone()));
        let (s, cid) = scene();
        let src = source(&s, &gpu);
        let oracle = {
            let mut r = Renderer::new(&src.project, src.footage.as_ref(), RenderOpts { backend: Backend::Gpu, ..Default::default() });
            r.accel = Some(&gpu as &dyn Accelerator);
            let f = gpu.render_display(&r, cid, Tick::ZERO).expect("GPU display frame");
            display_color_image(f.width, f.height, &gpu.read_display(&f).expect("readback")).expect("image")
        };
        let copied = Frames::default();
        let FrameImage::Cpu(copy) = render(&copied, &s, &src, cid, 0) else { panic!("the readback path caches pixels") };
        assert_eq!(copied.resident_frames(), 0);
        let resident = Frames::default();
        resident.set_resident(true);
        let FrameImage::Gpu(tex) = render(&resident, &s, &src, cid, 0) else { panic!("a resident frame stays a texture") };
        assert_eq!(resident.resident_frames(), 1);
        assert_eq!(resident.landing.landed(), 0, "no readback");
        let shown = display_color_image(tex.width, tex.height, &gpu.read_display(&tex).expect("readback")).expect("image");
        assert_eq!(shown.size, copy.size);
        assert_eq!(shown.pixels, copy.pixels, "the texture holds exactly the pixels the readback path shows");
        assert_eq!(shown.pixels, oracle.pixels);
        assert!(shown.pixels.iter().any(|p| p.a() > 0 && p.a() < 255), "partial alpha is covered");
        assert!(light.stats().reds == 0 && light.stats().late == 0);

        // Several frames in flight at once stay bounded and all finish.
        for frame in 1..8 {
            assert!(matches!(render(&resident, &s, &src, cid, frame), FrameImage::Gpu(_)));
        }
        assert!(resident.resident.flights.lock().unwrap().len() <= MAX_IN_FLIGHT);

        // Switched off (Settings, live): frames are read back again.
        resident.set_resident(false);
        assert!(resident.resident.flights.lock().unwrap().is_empty());
        assert!(matches!(render(&resident, &s, &src, cid, 9), FrameImage::Cpu(_)));
    }

    /// RT10e: a compositor that retires (device fault) renders its frames on the CPU, never as a
    /// texture of the failed device; GPU frames already cached are dropped.
    #[test]
    fn a_retired_compositor_falls_back_to_cpu_frames() {
        let Some(gpu) = Gpu::headless() else {
            assert!(std::env::var("EFFECTCRAFT_REQUIRE_GPU").as_deref() != Ok("1"), "GPU required");
            return;
        };
        let (s, cid) = scene();
        let src = source(&s, &gpu);
        let f = Frames::default();
        f.set_resident(true);
        assert!(matches!(render(&f, &s, &src, cid, 0), FrameImage::Gpu(_)));
        (gpu.context().device_failure_notifier())("synthetic device fault".into());
        assert!(gpu.context().check_health().is_err());
        let shown = f.resident_frames();
        assert!(matches!(render(&f, &s, &src, cid, 1), FrameImage::Cpu(_)), "the retired device's frame renders on the CPU");
        assert_eq!(f.resident_frames(), shown);
        f.retire_gpu();
        let key0 = FrameKey { revision: s.revision, content: f.content_of(&s.project, s.revision, cid), comp: cid.0, frame: 0, scale: 1000, view: 0, opts: 0 };
        assert!(f.get(&key0).is_none(), "GPU frames are dropped when the GPU preview retires");
        assert!(!f.resident.on(), "retiring turns resident frames off");
    }
}
