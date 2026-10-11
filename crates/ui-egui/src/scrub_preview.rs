//! Bounded timeline-ruler motion history; no project data or render work here.
const QUIET: f64 = 0.12;

#[derive(Clone, Copy, Debug)]
struct Sample {
    comp: u64,
    revision: u64,
    frame: i64,
    at: f64,
}

#[derive(Default)]
pub(crate) struct ScrubPreview {
    now: f64,
    sample: Option<Sample>,
    moving: bool,
    direction: i64,
    stride: i64,
}

impl ScrubPreview {
    pub fn tick(&mut self, now: f64, comp: Option<u64>, revision: u64) {
        if !now.is_finite() || now < self.now || self.sample.is_some_and(|s| Some(s.comp) != comp || s.revision != revision) {
            *self = Self::default();
        }
        self.now = if now.is_finite() { now } else { 0.0 };
    }

    pub fn observe(&mut self, comp: u64, revision: u64, frame: i64, now: f64) {
        self.tick(now, Some(comp), revision);
        if !now.is_finite() {
            return;
        }
        let next = Sample { comp, revision, frame, at: now };
        let Some(old) = self.sample else {
            self.sample = Some(next);
            return;
        };
        if old.frame == frame {
            return;
        }
        let dt = now - old.at;
        let delta = frame.saturating_sub(old.frame);
        let direction = delta.signum();
        self.moving = dt > 0.0 && dt <= QUIET;
        let speed = (delta.unsigned_abs() as f64 / dt.max(0.001)).min(400.0);
        let stride = (speed * 0.08).ceil().clamp(1.0, 32.0) as i64;
        self.stride = if self.direction != 0 && self.direction != direction { 1 } else { stride };
        self.direction = direction;
        self.sample = Some(next);
    }

    pub fn active(&self, comp: u64, revision: u64, frame: i64) -> bool {
        self.moving && self.matches(comp, revision, frame) && self.remaining().is_some()
    }

    pub fn matches(&self, comp: u64, revision: u64, frame: i64) -> bool {
        self.sample.is_some_and(|s| s.comp == comp && s.revision == revision && s.frame == frame)
    }

    pub fn remaining(&self) -> Option<f64> {
        let elapsed = self.now - self.sample?.at;
        (self.moving && (0.0..QUIET).contains(&elapsed)).then_some(QUIET - elapsed)
    }

    pub fn neighbors(&self, frame: i64, last: i64) -> Vec<i64> {
        if self.remaining().is_none() || last < 0 {
            return Vec::new();
        }
        let step = self.direction.saturating_mul(self.stride.max(1));
        [frame.saturating_add(step), frame.saturating_sub(step)].into_iter().filter(|f| *f >= 0 && *f <= last && *f != frame).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn movement_is_directional_bounded_and_refines_after_quiet() {
        let mut s = ScrubPreview::default();
        s.observe(1, 2, 10, 1.0);
        assert!(!s.active(1, 2, 10));
        s.observe(1, 2, 14, 1.05);
        assert!(s.active(1, 2, 14));
        assert_eq!(s.neighbors(14, 100), vec![21, 7]);
        s.observe(1, 2, 13, 1.10);
        assert_eq!(s.neighbors(13, 100), vec![12, 14]);
        s.tick(1.23, Some(1), 2);
        assert!(!s.active(1, 2, 13));
        assert!(s.matches(1, 2, 13));
        assert!(s.neighbors(13, 100).is_empty());
    }
    #[test]
    fn clicks_revisions_and_invalid_clocks_do_not_scrub() {
        let mut s = ScrubPreview::default();
        s.observe(1, 2, 10, 1.0);
        s.observe(1, 2, 20, 2.0);
        assert!(!s.active(1, 2, 20));
        s.observe(1, 2, 21, 2.01);
        assert!(s.active(1, 2, 21));
        s.tick(2.02, Some(1), 3);
        assert!(!s.matches(1, 2, 21));
        s.observe(1, 3, i64::MIN, 3.0);
        s.observe(1, 3, i64::MAX, 3.01);
        assert!(s.neighbors(i64::MAX, i64::MAX).len() <= 2);
        s.tick(f64::NAN, Some(1), 3);
        assert!(s.neighbors(0, 100).is_empty());
    }
    #[test]
    fn holding_the_pointer_does_not_extend_coarse_preview() {
        let mut s = ScrubPreview::default();
        s.observe(1, 1, 0, 1.0);
        s.observe(1, 1, 1, 1.01);
        s.observe(1, 1, 1, 1.5);
        assert!(!s.active(1, 1, 1));
    }
}
