//! Bounded native render-service measurements for Auto playback resolution.
//! Cache hits and old playback/content/scale epochs never train this controller.
//!
//! A ladder Full → Half → Quarter: three slow renders out of the last four step down one
//! rung, so a slow cut is caught a few frames in; eight renders that would fit the budget
//! comfortably at the rung above step back up. A rung that is not measurably faster than
//! the one above it is abandoned and becomes the floor for the rest of the epoch.
use std::collections::VecDeque;
use std::sync::Arc;

use crate::frames::FrameKey;

const WINDOW: usize = 32;
// AutoPick warms three frames per compositor (CPU and GPU). Discard those renders when
// playback starts, so cold uploads and compositor exploration do not decide.
const WARMUP: usize = 6;
// After a rung change only the first cold renders at the new scale are discarded.
const STEP_WARMUP: usize = 3;
/// Step down when `SLOW_OF` of the last `DOWN` renders miss the deadline.
const DOWN: usize = 4;
const SLOW_OF: usize = 3;
/// Renders measured at a new lower rung before judging whether it is faster.
const VERIFY: usize = 4;
/// Step up when the median of the last `UP` renders, scaled by the rung above's pixel count,
/// fits within `UP_HEADROOM` of the budget.
const UP: usize = 8;
const UP_HEADROOM: f64 = 0.75;

#[derive(Clone, Copy, Debug)]
pub(crate) struct Config {
    pub full: FrameKey,
    pub half: FrameKey,
    pub quarter: FrameKey,
    pub budget_ms: f64,
    pub gpu_context: usize,
}

impl Config {
    fn same(&self, other: &Self) -> bool {
        self.full.same_series(&other.full)
            && self.full.revision == other.full.revision
            && self.half.same_series(&other.half)
            && self.quarter.same_series(&other.quarter)
            && self.budget_ms == other.budget_ms
            && self.gpu_context == other.gpu_context
    }

    fn rung(&self, level: usize) -> FrameKey {
        match level {
            0 => self.full,
            1 => self.half,
            _ => self.quarter,
        }
    }

    /// The lowest usable rung: Quarter only when it is smaller than Half.
    fn bottom(&self) -> usize {
        if self.quarter.scale < self.half.scale { 2 } else { 1 }
    }
}

#[derive(Clone)]
pub(crate) struct Ticket(Arc<()>);

#[derive(Default)]
pub(crate) struct Budget {
    config: Option<Config>,
    epoch: Arc<()>,
    samples: VecDeque<f64>,
    residency: Option<bool>,
    /// The rung in use: 0 Full, 1 Half, 2 Quarter.
    level: usize,
    /// Lowest rung allowed this epoch (a rung that was not faster raises it).
    floor: usize,
    /// Median at the rung above, while the current rung is being verified.
    trial_reference: Option<f64>,
    warmup: usize,
    trial_samples: usize,
}

fn median(samples: impl Iterator<Item = f64>) -> f64 {
    let mut sorted: Vec<_> = samples.collect();
    sorted.sort_by(f64::total_cmp);
    sorted.get(sorted.len() / 2).copied().unwrap_or(0.0)
}

impl Budget {
    /// The rung to render at (0 Full, 1 Half, 2 Quarter). A new playback/content epoch, or
    /// no adaptive playback at all (idle, an explicit resolution), restores Full.
    pub fn configure(&mut self, config: Option<Config>) -> usize {
        let config = config.filter(|c| c.budget_ms.is_finite() && c.budget_ms > 0.0 && c.half.scale < c.full.scale);
        let unchanged = match (&self.config, &config) {
            (Some(a), Some(b)) => a.same(b),
            (None, None) => true,
            _ => false,
        };
        if !unchanged {
            self.config = config;
            self.level = 0;
            self.floor = config.map_or(0, |c| c.bottom());
            self.trial_reference = None;
            self.trial_samples = 0;
            self.reset_samples(WARMUP);
        }
        self.level
    }

    fn reset_samples(&mut self, warmup: usize) {
        self.epoch = Arc::new(());
        self.samples.clear();
        self.residency = None;
        self.warmup = warmup;
    }

    fn step(&mut self, level: usize) {
        self.level = level;
        self.trial_samples = 0;
        self.reset_samples(STEP_WARMUP);
    }

    pub fn ticket(&self, key: FrameKey) -> Option<Ticket> {
        let expected = self.config.as_ref()?.rung(self.level);
        (key.same_series(&expected) && key.revision == expected.revision).then(|| Ticket(self.epoch.clone()))
    }

    pub fn record(&mut self, ticket: Ticket, ms: f64, resident: bool) {
        if !Arc::ptr_eq(&ticket.0, &self.epoch) || !ms.is_finite() || ms <= 0.0 {
            return;
        }
        let Some(config) = self.config else { return };
        if self.trial_reference.is_some() {
            self.trial_samples = self.trial_samples.saturating_add(1);
            if self.trial_samples > 64 {
                // An unstable trial never settled: return to the rung above and stay there.
                self.trial_reference = None;
                self.floor = self.level.saturating_sub(1);
                self.step(self.floor);
                return;
            }
        }
        if self.residency.is_some_and(|old| old != resident) {
            self.samples.clear();
            self.warmup = STEP_WARMUP;
        }
        self.residency = Some(resident);
        if self.warmup > 0 {
            self.warmup = self.warmup.saturating_sub(1);
            return;
        }
        if self.samples.len() >= WINDOW {
            self.samples.pop_front();
        }
        self.samples.push_back(ms);
        let n = self.samples.len();
        if let Some(reference) = self.trial_reference {
            if n < VERIFY {
                return;
            }
            self.trial_reference = None;
            // Fewer pixels are not necessarily faster: Auto may choose different compositors
            // at each scale. Keep the lower rung only with a clear measured win.
            if median(self.samples.iter().copied()) >= reference * 0.9 {
                self.floor = self.level.saturating_sub(1);
                self.step(self.floor);
                return;
            }
        }
        let threshold = config.budget_ms * 1.1;
        let recent = self.samples.iter().skip(n.saturating_sub(DOWN));
        if self.level < self.floor && n >= DOWN && recent.filter(|ms| **ms > threshold).count() >= SLOW_OF {
            let reference = median(self.samples.iter().skip(n.saturating_sub(DOWN)).copied());
            self.step(self.level.saturating_add(1));
            self.trial_reference = Some(reference);
            return;
        }
        if self.level > 0 && n >= UP {
            // Cost grows with the pixel count: predict the rung above from this one.
            let above = config.rung(self.level.saturating_sub(1)).scale as f64;
            let here = config.rung(self.level).scale.max(1) as f64;
            let predicted = median(self.samples.iter().skip(n.saturating_sub(UP)).copied()) * (above / here).powi(2);
            if predicted <= config.budget_ms * UP_HEADROOM {
                self.step(self.level.saturating_sub(1));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn config() -> Config {
        let full = FrameKey { revision: 1, content: 1, comp: 1, frame: 0, scale: 1000, view: 0, opts: 1 };
        Config {
            full,
            half: FrameKey { scale: 500, opts: 2, ..full },
            quarter: FrameKey { scale: 250, opts: 3, ..full },
            budget_ms: 1000.0 / 30.0,
            gpu_context: 0,
        }
    }
    /// Record a render at the rung in use (`false` when no ticket is issued for it).
    fn at(b: &mut Budget, key: FrameKey, ms: f64, gpu: bool) -> bool {
        let Some(t) = b.ticket(key) else { return false };
        b.record(t, ms, gpu);
        true
    }
    fn sample(b: &mut Budget, c: Config, ms: f64, gpu: bool) {
        assert!(at(b, c.full, ms, gpu));
    }
    fn warm(b: &mut Budget, c: Config) {
        for _ in 0..WARMUP {
            sample(b, c, 10.0, false);
        }
    }

    #[test]
    fn slow_stretch_reduces_within_four_renders() {
        let c = config();
        let mut b = Budget::default();
        assert_eq!(b.configure(Some(c)), 0);
        warm(&mut b, c);
        for _ in 0..8 {
            sample(&mut b, c, 10.0, false);
        }
        for _ in 0..SLOW_OF - 1 {
            sample(&mut b, c, 50.0, false);
        }
        assert_eq!(b.configure(Some(c)), 0);
        sample(&mut b, c, 50.0, false);
        assert_eq!(b.configure(Some(c)), 1);
        assert!(b.ticket(c.full).is_none());
        assert!(b.ticket(c.half).is_some());
        // Idle (or any explicit choice, which configures nothing) restores Full.
        assert_eq!(b.configure(None), 0);
        assert_eq!(b.configure(Some(c)), 0);
    }

    #[test]
    fn ladder_steps_to_quarter_while_half_is_slow_and_back_up_when_comfortable() {
        let c = config();
        let mut b = Budget::default();
        b.configure(Some(c));
        warm(&mut b, c);
        for _ in 0..DOWN {
            sample(&mut b, c, 100.0, false);
        }
        assert_eq!(b.configure(Some(c)), 1);
        // Half is faster, but still over budget: on to Quarter.
        for _ in 0..STEP_WARMUP + DOWN {
            assert!(at(&mut b, c.half, 60.0, false));
        }
        assert_eq!(b.configure(Some(c)), 2);
        // The cut ends: Quarter renders that would fit at Half step up, then up again.
        for _ in 0..STEP_WARMUP + UP {
            assert!(at(&mut b, c.quarter, 3.0, false));
        }
        assert_eq!(b.configure(Some(c)), 1);
        for _ in 0..STEP_WARMUP + UP {
            assert!(at(&mut b, c.half, 5.0, false));
        }
        assert_eq!(b.configure(Some(c)), 0);
        // Renders that would only just fit above stay down (no oscillation).
        for _ in 0..STEP_WARMUP + DOWN {
            sample(&mut b, c, 100.0, false);
        }
        assert_eq!(b.configure(Some(c)), 1);
        for _ in 0..STEP_WARMUP + 100 {
            assert!(at(&mut b, c.half, 8.0, false));
        }
        assert_eq!(b.configure(Some(c)), 1);
    }

    #[test]
    fn quarter_is_skipped_when_it_is_not_smaller_than_half() {
        let mut c = config();
        c.quarter = c.half;
        let mut b = Budget::default();
        b.configure(Some(c));
        warm(&mut b, c);
        for _ in 0..DOWN {
            sample(&mut b, c, 100.0, false);
        }
        assert_eq!(b.configure(Some(c)), 1);
        for _ in 0..100 {
            assert!(at(&mut b, c.half, 60.0, false));
        }
        assert_eq!(b.configure(Some(c)), 1);
    }

    #[test]
    fn explicit_resolution_and_invalid_budgets_never_adapt() {
        let c = config();
        let mut b = Budget::default();
        // An explicit resolution, Fast Previews off or export configure nothing.
        assert_eq!(b.configure(None), 0);
        assert!(b.ticket(c.full).is_none());
        for budget_ms in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert_eq!(b.configure(Some(Config { budget_ms, ..c })), 0);
            assert!(b.ticket(c.full).is_none());
        }
        // A Half no smaller than Full (the adaptive limit) is no ladder either.
        let flat = Config { half: c.full, quarter: c.full, ..c };
        assert_eq!(b.configure(Some(flat)), 0);
        assert!(b.ticket(c.full).is_none());
    }

    #[test]
    fn one_hitch_invalid_samples_and_cache_only_polls_do_not_reduce() {
        let c = config();
        let mut b = Budget::default();
        b.configure(Some(c));
        for ms in [f64::NAN, f64::INFINITY, -1.0, 0.0] {
            sample(&mut b, c, ms, false);
        }
        for _ in 0..31 {
            sample(&mut b, c, 10.0, false);
        }
        sample(&mut b, c, 100.0, false);
        for _ in 0..100 {
            assert_eq!(b.configure(Some(c)), 0);
        }
        sample(&mut b, c, 100.0, false);
        assert_eq!(b.configure(Some(c)), 0);
    }

    #[test]
    fn stale_epochs_series_revisions_and_backend_history_are_rejected() {
        let c = config();
        let mut b = Budget::default();
        b.configure(Some(c));
        let old = b.ticket(c.full).unwrap();
        b.configure(None);
        b.configure(Some(c));
        b.record(old, 1000.0, false);
        assert!(b.samples.is_empty());
        assert!(b.ticket(FrameKey { revision: 2, ..c.full }).is_none());
        assert!(b.ticket(FrameKey { content: 2, ..c.full }).is_none());
        warm(&mut b, c);
        for _ in 0..DOWN - 1 {
            sample(&mut b, c, 100.0, false);
        }
        sample(&mut b, c, 10.0, true);
        assert!(b.samples.is_empty());
        assert_eq!(b.configure(Some(c)), 0);
        let changed = Config { full: FrameKey { content: 2, ..c.full }, ..c };
        assert_eq!(b.configure(Some(changed)), 0);
        assert!(b.samples.is_empty());
    }

    #[test]
    fn slower_half_trial_restores_full_without_repeated_trials() {
        let c = config();
        let mut b = Budget::default();
        b.configure(Some(c));
        warm(&mut b, c);
        for _ in 0..DOWN {
            sample(&mut b, c, 50.0, false);
        }
        assert_eq!(b.configure(Some(c)), 1);
        for _ in 0..STEP_WARMUP + VERIFY {
            assert!(at(&mut b, c.half, 80.0, false));
        }
        assert_eq!(b.configure(Some(c)), 0);
        for _ in 0..100 {
            sample(&mut b, c, 1000.0, false);
        }
        assert_eq!(b.configure(Some(c)), 0);
        b.configure(None);
        b.configure(Some(c));
        warm(&mut b, c);
        for _ in 0..DOWN {
            sample(&mut b, c, 50.0, false);
        }
        assert_eq!(b.configure(Some(c)), 1);
    }

    #[test]
    fn cold_compositor_trials_do_not_choose_resolution() {
        let c = config();
        let mut b = Budget::default();
        b.configure(Some(c));
        for _ in 0..WARMUP {
            sample(&mut b, c, 1000.0, false);
        }
        for _ in 0..UP {
            sample(&mut b, c, 10.0, false);
        }
        assert_eq!(b.configure(Some(c)), 0);
    }

    #[test]
    fn sporadic_missed_deadlines_do_not_reduce() {
        let c = config();
        let mut b = Budget::default();
        b.configure(Some(c));
        warm(&mut b, c);
        for i in 0..64 {
            sample(&mut b, c, if i % 2 == 0 { 60.0 } else { 20.0 }, false);
        }
        assert_eq!(b.configure(Some(c)), 0);
    }

    #[test]
    fn unstable_trial_is_bounded_and_old_half_completion_cannot_retrain_full() {
        let c = config();
        let mut b = Budget::default();
        b.configure(Some(c));
        warm(&mut b, c);
        for _ in 0..DOWN {
            sample(&mut b, c, 50.0, false);
        }
        let late = b.ticket(c.half).unwrap();
        for i in 0..65 {
            assert!(at(&mut b, c.half, 80.0, i % 2 == 0));
        }
        assert_eq!(b.configure(Some(c)), 0);
        b.record(late, 1.0, false);
        assert!(b.samples.is_empty());
    }
}
