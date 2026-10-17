//! CR1: how many viewer frames composite on the GPU at once, from the video memory they turn out
//! to take.
//!
//! A frame's working set on the GPU (canvases, precomps, effect temporaries) can be ten times the
//! four frame-sized buffers [`super::max_running`] estimates: sixteen 1080p frames at once filled
//! a 20 GB card. So on a discrete GPU:
//! - the frames share what was free of its video memory when the device was made, at most three
//!   quarters of it ([`effectcraft_gpu::vram`]; 2 GiB when it is unknown), less what the device
//!   holds with no frame rendering (caches, the texture pool);
//! - where wgpu reports the device's allocations (Vulkan, Direct3D 12), each finished frame
//!   measures what the frames rendering took per frame, and the cap follows that;
//! - the first frames start two at a time, then twice as many as have finished (a slow start), so
//!   the measurement comes before many frames run at once;
//! - each time the GPU runs out of memory, at most half as many frames as were rendering then
//!   run from then on, also on a compositor that replaces a failed one.
//!
//! Integrated GPUs (unified memory) keep the RAM-based caps.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use effectcraft_gpu::Gpu;

/// Frame working budget on a discrete GPU whose video memory is unknown.
pub(crate) const UNKNOWN_VRAM_BUDGET: u64 = 2 << 30;

/// Measure the device at most this often (it walks every allocation).
const SAMPLE_EVERY: std::time::Duration = std::time::Duration::from_millis(100);

/// The adaptive GPU frame cap ([`Gate::limit`]), shared by the queue and the frame workers.
pub(crate) struct Gate {
    /// GPU frames rendering now, and the most at once since the compositor started.
    rendering: Arc<AtomicUsize>,
    peak: AtomicUsize,
    /// At most this many after running out of memory (`usize::MAX`: no limit yet).
    ceiling: AtomicUsize,
    /// GPU frames finished on this compositor (the slow start).
    finished: AtomicUsize,
    /// Bytes a GPU frame takes on the device, as measured (0: not yet).
    per_frame: AtomicU64,
    /// Device bytes with no GPU frame rendering (`u64::MAX`: not measured).
    baseline: AtomicU64,
    /// The device reports its allocations: 0 not known yet, 1 yes, 2 no.
    measurable: std::sync::atomic::AtomicU8,
    last_sample: Mutex<Option<web_time::Instant>>,
}

impl Default for Gate {
    fn default() -> Self {
        Gate {
            rendering: Arc::default(),
            peak: AtomicUsize::new(0),
            ceiling: AtomicUsize::new(usize::MAX),
            finished: AtomicUsize::new(0),
            per_frame: AtomicU64::new(0),
            baseline: AtomicU64::new(u64::MAX),
            measurable: std::sync::atomic::AtomicU8::new(0),
            last_sample: Mutex::new(None),
        }
    }
}

/// A GPU frame rendering ([`Gate::enter`]); counted out when dropped.
pub(crate) struct Entered<'a> {
    gate: &'a Gate,
    gpu: Gpu,
}

impl Drop for Entered<'_> {
    fn drop(&mut self) {
        // Paired with `enter`'s increment.
        if self.gate.rendering.fetch_sub(1, Ordering::AcqRel) == 1 {
            // Nothing rendering: what the device holds now is the baseline.
            if let Some(bytes) = self.gate.sample(&self.gpu) {
                self.gate.baseline.store(bytes, Ordering::Release);
            }
        }
    }
}

impl Gate {
    /// GPU frames that may render at once on `g` when one takes at least `frame` bytes (the
    /// static estimate), with `threads` frame threads. `None` on an integrated GPU (or another
    /// without video memory of its own): the caller keeps its RAM-based cap, under
    /// [`Gate::ceiling`].
    pub(crate) fn limit(&self, g: &Gpu, frame: f64, threads: usize) -> Option<usize> {
        let ctx = g.context();
        let vram = ctx.video_memory();
        if vram.is_none() && !ctx.discrete() {
            return None;
        }
        let baseline = match self.baseline.load(Ordering::Acquire) {
            u64::MAX => 0,
            b => b,
        };
        // What was free when the device was made (another program may use the rest), less a
        // quarter of the card for headroom, less what this device holds anyway.
        let usable = vram.map_or(UNKNOWN_VRAM_BUDGET, |m| m.free.min(m.total / 4 * 3).saturating_sub(baseline).max(m.total / 16));
        let per = (self.per_frame.load(Ordering::Acquire) as f64).max(frame).max(1.0);
        let mut n = ((usable as f64 / per) as usize).clamp(1, threads.max(1));
        if self.measurable.load(Ordering::Acquire) != 2 {
            n = n.min(self.finished.load(Ordering::Acquire).saturating_mul(2).max(2));
        }
        Some(n.min(self.ceiling()))
    }

    /// At most this many GPU frames at once after running out of memory (`usize::MAX`: no limit).
    pub(crate) fn ceiling(&self) -> usize {
        self.ceiling.load(Ordering::Acquire).max(1)
    }

    /// A GPU frame starts rendering on `g`.
    pub(crate) fn enter(&self, g: &Gpu) -> Entered<'_> {
        if self.baseline.load(Ordering::Acquire) == u64::MAX
            && self.rendering.load(Ordering::Acquire) == 0
            && let Some(bytes) = self.sample(g)
        {
            self.baseline.store(bytes, Ordering::Release);
        }
        let n = self.rendering.fetch_add(1, Ordering::AcqRel).saturating_add(1);
        self.peak.fetch_max(n, Ordering::AcqRel);
        Entered { gate: self, gpu: g.clone() }
    }

    /// The frame `e` rendered on the GPU: measure what the frames rendering take each.
    pub(crate) fn finished(&self, e: &Entered<'_>) {
        self.finished.fetch_add(1, Ordering::AcqRel);
        let n = self.rendering.load(Ordering::Acquire).max(1) as u64;
        let (Some(bytes), baseline) = (self.sample(&e.gpu), self.baseline.load(Ordering::Acquire)) else { return };
        if baseline == u64::MAX {
            return;
        }
        let per = bytes.saturating_sub(baseline) / n;
        // The largest recent measurement, forgotten slowly (an eighth of the difference each
        // time): a cap that follows a heavier stretch of the comp at once, and a lighter one later.
        let old = self.per_frame.load(Ordering::Acquire);
        self.per_frame.store(if per >= old { per } else { old - (old - per) / 8 }, Ordering::Release);
    }

    /// A GPU frame ran out of memory with the frames rendering now: at most half as many from
    /// now on.
    pub(crate) fn out_of_memory(&self) {
        self.lower(self.rendering.load(Ordering::Acquire));
    }

    /// The compositor failed (retired): its replacement runs at most half as many GPU frames as
    /// ran at once on it.
    pub(crate) fn retired(&self) {
        self.lower(self.peak.load(Ordering::Acquire));
    }

    fn lower(&self, running: usize) {
        if running == 0 {
            // Nothing rendered on it: no measure of what ran out.
            return;
        }
        let half = (running / 2).max(1);
        let before = self.ceiling.fetch_min(half, Ordering::AcqRel);
        if half < before {
            log::warn!("the GPU ran out of memory with {running} frames rendering on it: at most {half} at once from now on");
        }
    }

    /// A new compositor (a replacement device): measure it from scratch, keeping the ceiling.
    pub(crate) fn revived(&self) {
        self.finished.store(0, Ordering::Release);
        self.per_frame.store(0, Ordering::Release);
        self.baseline.store(u64::MAX, Ordering::Release);
        self.peak.store(self.rendering.load(Ordering::Acquire), Ordering::Release);
        self.measurable.store(0, Ordering::Release);
    }

    /// Device bytes now, at most every [`SAMPLE_EVERY`] (`None` between, or when the device
    /// doesn't report them).
    fn sample(&self, g: &Gpu) -> Option<u64> {
        if self.measurable.load(Ordering::Acquire) == 2 {
            return None;
        }
        {
            let mut last = self.last_sample.lock().unwrap_or_else(PoisonError::into_inner);
            let now = web_time::Instant::now();
            if last.is_some_and(|t| now.duration_since(t) < SAMPLE_EVERY) && self.measurable.load(Ordering::Acquire) == 1 {
                return None;
            }
            *last = Some(now);
        }
        let bytes = g.context().device_bytes();
        self.measurable.store(if bytes.is_some() { 1 } else { 2 }, Ordering::Release);
        bytes
    }

    /// For `ui.inspect` / tests: (ceiling, learned bytes per frame, frames rendering).
    pub(crate) fn state(&self) -> (Option<usize>, u64, usize) {
        let c = self.ceiling.load(Ordering::Acquire);
        ((c != usize::MAX).then_some(c), self.per_frame.load(Ordering::Acquire), self.rendering.load(Ordering::Acquire))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_out_of_memory_halves_the_frames_rendering_down_to_one() {
        let g = Gate::default();
        assert_eq!(g.ceiling(), usize::MAX);
        g.rendering.store(16, Ordering::Release);
        g.out_of_memory();
        assert_eq!(g.ceiling(), 8);
        g.rendering.store(12, Ordering::Release);
        g.out_of_memory();
        assert_eq!(g.ceiling(), 6, "half of those rendering, never above the ceiling");
        g.rendering.store(30, Ordering::Release);
        g.out_of_memory();
        assert_eq!(g.ceiling(), 6, "a ceiling never rises");
        for _ in 0..10 {
            g.rendering.store(g.ceiling(), Ordering::Release);
            g.out_of_memory();
        }
        assert_eq!(g.ceiling(), 1);
    }

    #[test]
    fn a_retired_compositor_halves_the_most_that_ran_and_its_replacement_keeps_it() {
        let g = Gate::default();
        g.peak.store(16, Ordering::Release);
        g.finished.store(40, Ordering::Release);
        g.per_frame.store(1 << 30, Ordering::Release);
        g.retired();
        g.revived();
        assert_eq!(g.ceiling(), 8);
        assert_eq!((g.finished.load(Ordering::Acquire), g.per_frame.load(Ordering::Acquire)), (0, 0), "the new device is measured anew");
    }
}
