//! Bounded native render-service measurements for Auto playback resolution.
//! Cache hits and old playback/content/scale epochs never train this controller.
use std::collections::VecDeque;
use std::sync::Arc;

use crate::frames::FrameKey;

const WINDOW: usize = 32;
const MIN_SAMPLES: usize = 8;
// AutoPick warms three frames per compositor. Discard eight actual renders
// at each new scale so cold uploads and compositor exploration do not decide.
const WARMUP: usize = 8;

#[derive(Clone, Copy, Debug)]
pub(crate) struct Config {
    pub full: FrameKey,
    pub half: FrameKey,
    pub budget_ms: f64,
    pub gpu_context: usize,
}

impl Config {
    fn same(&self, other: &Self) -> bool {
        self.full.same_series(&other.full)
            && self.full.revision == other.full.revision
            && self.half.same_series(&other.half)
            && self.budget_ms == other.budget_ms
            && self.gpu_context == other.gpu_context
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
    reduced: bool,
    trial_reference: Option<f64>,
    locked_full: bool,
    warmup: usize,
    trial_samples: usize,
}

impl Budget {
    /// Trial Half once per playback/content epoch. Keep it only when measured
    /// faster; do not oscillate on cache hits. Idle always restores Full.
    pub fn configure(&mut self, config: Option<Config>) -> bool {
        let config = config.filter(|c| c.budget_ms.is_finite() && c.budget_ms > 0.0 && c.half.scale < c.full.scale);
        let unchanged = match (&self.config, &config) {
            (Some(a), Some(b)) => a.same(b),
            (None, None) => true,
            _ => false,
        };
        if !unchanged {
            self.config = config;
            self.reduced = false;
            self.trial_reference = None;
            self.locked_full = false;
            self.trial_samples = 0;
            self.reset_samples();
        }
        self.reduced
    }

    fn reset_samples(&mut self) {
        self.epoch = Arc::new(());
        self.samples.clear();
        self.residency = None;
        self.warmup = WARMUP;
    }

    pub fn ticket(&self, key: FrameKey) -> Option<Ticket> {
        let c = self.config.as_ref()?;
        let expected = if self.reduced { c.half } else { c.full };
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
                self.reduced = false;
                self.locked_full = true;
                self.trial_reference = None;
                self.reset_samples();
                return;
            }
        }
        if self.residency.is_some_and(|old| old != resident) {
            self.samples.clear();
            self.warmup = WARMUP;
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
        if self.locked_full || (self.reduced && self.trial_reference.is_none()) || self.samples.len() < MIN_SAMPLES {
            return;
        }
        let mut sorted: Vec<_> = self.samples.iter().copied().collect();
        sorted.sort_by(f64::total_cmp);
        // Nearest-rank p95. The bounded length makes these operations safe.
        let rank = sorted.len().saturating_mul(95).div_ceil(100).saturating_sub(1);
        let p95 = sorted.get(rank).copied().unwrap_or(0.0);
        if let Some(reference) = self.trial_reference.take() {
            // Fewer pixels are not necessarily faster: Auto may choose different
            // compositors at each scale. Keep Half only with a clear measured win.
            if p95 >= reference * 0.9 {
                self.reduced = false;
                self.locked_full = true;
                self.reset_samples();
            }
            return;
        }
        // Resolution trials themselves have a cost. Require sustained missed
        // deadlines (three quarters of the window), not just a few p95 outliers.
        let threshold = config.budget_ms * 1.1;
        let required = sorted.len().saturating_mul(3).div_ceil(4);
        if p95 > threshold && sorted.iter().filter(|ms| **ms > threshold).count() >= required {
            self.reduced = true;
            self.trial_reference = Some(p95);
            self.trial_samples = 0;
            self.reset_samples();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn config() -> Config {
        let full = FrameKey { revision: 1, content: 1, comp: 1, frame: 0, scale: 1000, view: 0, opts: 1 };
        Config { full, half: FrameKey { scale: 500, opts: 2, ..full }, budget_ms: 1000.0 / 30.0, gpu_context: 0 }
    }
    fn sample(b: &mut Budget, c: Config, ms: f64, gpu: bool) {
        let t = b.ticket(c.full).unwrap();
        b.record(t, ms, gpu);
    }
    #[test]
    fn slow_p95_reduces_only_after_enough_real_samples_and_idle_restores() {
        let c = config();
        let mut b = Budget::default();
        assert!(!b.configure(Some(c)));
        for _ in 0..WARMUP { sample(&mut b, c, 50.0, true); }
        for _ in 0..7 { sample(&mut b, c, 50.0, true); }
        assert!(!b.configure(Some(c)));
        sample(&mut b, c, 50.0, true);
        assert!(b.configure(Some(c)));
        assert!(b.ticket(c.full).is_none());
        for _ in 0..100 {
            let ticket = b.ticket(c.half).unwrap();
            b.record(ticket, 2.0, true);
        }
        assert!(b.configure(Some(c)));
        assert_eq!(b.samples.len(), WINDOW);
        assert!(!b.configure(None));
        assert!(!b.configure(Some(c)));
    }
    #[test]
    fn one_hitch_invalid_samples_and_cache_only_polls_do_not_reduce() {
        let c = config();
        let mut b = Budget::default();
        b.configure(Some(c));
        for ms in [f64::NAN, f64::INFINITY, -1.0, 0.0] { sample(&mut b, c, ms, false); }
        for _ in 0..31 { sample(&mut b, c, 10.0, false); }
        sample(&mut b, c, 100.0, false);
        for _ in 0..100 { assert!(!b.configure(Some(c))); }
        sample(&mut b, c, 100.0, false);
        assert!(!b.configure(Some(c)));
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
        for _ in 0..WARMUP { sample(&mut b, c, 10.0, false); }
        for _ in 0..7 { sample(&mut b, c, 100.0, false); }
        sample(&mut b, c, 10.0, true);
        assert!(b.samples.is_empty());
        assert!(!b.configure(Some(c)));
        let changed = Config { full: FrameKey { content: 2, ..c.full }, ..c };
        assert!(!b.configure(Some(changed)));
        assert!(b.samples.is_empty());
    }
    #[test]
    fn slower_half_trial_restores_full_without_repeated_trials() {
        let c = config();
        let mut b = Budget::default();
        b.configure(Some(c));
        for _ in 0..WARMUP + MIN_SAMPLES { sample(&mut b, c, 50.0, false); }
        assert!(b.configure(Some(c)));
        for _ in 0..WARMUP + MIN_SAMPLES {
            let ticket = b.ticket(c.half).unwrap();
            b.record(ticket, 80.0, false);
        }
        assert!(!b.configure(Some(c)));
        for _ in 0..100 { sample(&mut b, c, 1000.0, false); }
        assert!(!b.configure(Some(c)));
        b.configure(None);
        b.configure(Some(c));
        for _ in 0..WARMUP + MIN_SAMPLES { sample(&mut b, c, 50.0, false); }
        assert!(b.configure(Some(c)));
    }
    #[test]
    fn cold_compositor_trials_do_not_choose_resolution() {
        let c = config();
        let mut b = Budget::default();
        b.configure(Some(c));
        for _ in 0..WARMUP { sample(&mut b, c, 1000.0, false); }
        for _ in 0..MIN_SAMPLES { sample(&mut b, c, 10.0, false); }
        assert!(!b.configure(Some(c)));
    }

    #[test]
    fn sporadic_missed_deadlines_do_not_pay_for_a_resolution_trial() {
        let c = config();
        let mut b = Budget::default();
        b.configure(Some(c));
        for _ in 0..WARMUP { sample(&mut b, c, 1000.0, false); }
        for i in 0..64 { sample(&mut b, c, if matches!(i % 8, 0 | 2 | 3 | 5 | 6) { 60.0 } else { 20.0 }, false); }
        assert!(!b.configure(Some(c)));
    }

    #[test]
    fn unstable_trial_is_bounded_and_old_half_completion_cannot_retrain_full() {
        let c = config();
        let mut b = Budget::default();
        b.configure(Some(c));
        for _ in 0..WARMUP + MIN_SAMPLES { sample(&mut b, c, 50.0, false); }
        let late = b.ticket(c.half).unwrap();
        for i in 0..65 {
            let ticket = b.ticket(c.half).unwrap();
            b.record(ticket, 80.0, i % 2 == 0);
        }
        assert!(!b.configure(Some(c)));
        b.record(late, 1.0, false);
        assert!(b.samples.is_empty());
    }

}
