//! The traffic light between a window's surface and the compositor's submissions (RT10e).
//!
//! When the compositor shares the window's wgpu device (so viewer frames are drawn straight from
//! their textures, with no readback and no upload), `Surface::configure` (a window resize, a move
//! to another display, a new window) waits for that device to go idle and then fails with
//! `GpuWaitTimeout` if anything was submitted while it waited. The window's event path turns the
//! light red around everything that may configure a surface ([`TrafficLight::red`]): that lets
//! the submissions already under way finish and holds new ones ([`TrafficLight::pass`]) until it
//! turns green again. The thread holding the light red submits freely (it is the one that
//! configures, and it may composite a frame on the way).
//!
//! Every wait is bounded, so a light left red never freezes the window or the frame workers:
//! past [`TrafficLight::MAX_HOLD`] a held submission goes ahead (counted in
//! [`TrafficLight::stats`]); a configure it races then fails, which the device's error handler
//! reports as a retired GPU preview, never a crash.

use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::ThreadId;
use std::time::{Duration, Instant};

/// Counts for diagnostics and tests.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LightStats {
    /// Times the light turned red (outermost holds only).
    pub reds: u64,
    /// Submissions that waited for green.
    pub held: u64,
    /// Submissions that went ahead while red after waiting [`TrafficLight::MAX_HOLD`].
    pub late: u64,
    /// Reds that stopped waiting for submissions under way after [`TrafficLight::MAX_DRAIN`].
    pub undrained: u64,
}

#[derive(Default)]
struct State {
    /// Threads holding the light red, with how many holds each (holds nest).
    holders: Vec<(ThreadId, u32)>,
    /// Submissions under way.
    passing: usize,
    stats: LightStats,
}

/// See the [module docs](self). Green until something holds it red.
pub struct TrafficLight {
    state: Mutex<State>,
    cv: Condvar,
    max_hold: Duration,
    max_drain: Duration,
}

impl Default for TrafficLight {
    fn default() -> Self {
        Self::with_bounds(Self::MAX_HOLD, Self::MAX_DRAIN)
    }
}

impl std::fmt::Debug for TrafficLight {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TrafficLight").field("red", &self.is_red()).field("stats", &self.stats()).finish()
    }
}

/// Holds the light red until dropped ([`TrafficLight::red`]).
#[must_use = "the light turns green again when this is dropped"]
pub struct Red {
    light: Arc<TrafficLight>,
}

impl Drop for Red {
    fn drop(&mut self) {
        self.light.release();
    }
}

impl TrafficLight {
    /// The longest a submission waits for green.
    pub const MAX_HOLD: Duration = Duration::from_secs(2);
    /// The longest turning red waits for the submissions under way to finish.
    pub const MAX_DRAIN: Duration = Duration::from_millis(500);

    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// A light with other bounds (tests).
    pub fn with_bounds(max_hold: Duration, max_drain: Duration) -> Self {
        Self { state: Mutex::default(), cv: Condvar::new(), max_hold, max_drain }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Turn the light red on this thread until the returned guard drops: returns once the
    /// submissions already under way (on other threads) have been made, or after
    /// [`Self::MAX_DRAIN`]. Holds nest; the light is green again when the last one drops.
    pub fn red(self: &Arc<Self>) -> Red {
        self.hold();
        Red { light: self.clone() }
    }

    fn hold(&self) {
        let me = std::thread::current().id();
        let mut s = self.lock();
        if let Some(h) = s.holders.iter_mut().find(|(t, _)| *t == me) {
            h.1 = h.1.saturating_add(1);
            return;
        }
        s.holders.push((me, 1));
        s.stats.reds = s.stats.reds.saturating_add(1);
        let deadline = Instant::now() + self.max_drain;
        while s.passing > 0 {
            let Some(left) = deadline.checked_duration_since(Instant::now()).filter(|d| !d.is_zero()) else {
                s.stats.undrained = s.stats.undrained.saturating_add(1);
                log::warn!("gpu traffic light: {} submission(s) still under way after {:?}", s.passing, self.max_drain);
                break;
            };
            s = self.cv.wait_timeout(s, left).unwrap_or_else(PoisonError::into_inner).0;
        }
    }

    fn release(&self) {
        let me = std::thread::current().id();
        let mut s = self.lock();
        if let Some(i) = s.holders.iter().position(|(t, _)| *t == me) {
            let n = s.holders.get(i).map_or(0, |h| h.1.saturating_sub(1));
            if n == 0 {
                s.holders.swap_remove(i);
            } else if let Some(h) = s.holders.get_mut(i) {
                h.1 = n;
            }
        } else if !s.holders.is_empty() {
            // A guard dropped on another thread than the one that took it: release that hold
            // rather than leave the light red for good.
            if let Some(h) = s.holders.first_mut() {
                h.1 = h.1.saturating_sub(1);
            }
            s.holders.retain(|h| h.1 > 0);
        }
        drop(s);
        self.cv.notify_all();
    }

    /// Whether something holds the light red.
    pub fn is_red(&self) -> bool {
        !self.lock().holders.is_empty()
    }

    pub fn stats(&self) -> LightStats {
        self.lock().stats
    }

    /// Make a submission (`submit`) once the light is green, or at once on the thread holding it
    /// red. Waits at most [`Self::MAX_HOLD`].
    pub fn pass<R>(&self, submit: impl FnOnce() -> R) -> R {
        let me = std::thread::current().id();
        {
            let mut s = self.lock();
            if !s.holders.iter().any(|(t, _)| *t == me) {
                let deadline = Instant::now() + self.max_hold;
                let mut waited = false;
                while !s.holders.is_empty() {
                    let Some(left) = deadline.checked_duration_since(Instant::now()).filter(|d| !d.is_zero()) else {
                        s.stats.late = s.stats.late.saturating_add(1);
                        log::warn!("gpu traffic light: red for over {:?}; submitting anyway", self.max_hold);
                        break;
                    };
                    waited = true;
                    s = self.cv.wait_timeout(s, left).unwrap_or_else(PoisonError::into_inner).0;
                }
                if waited {
                    s.stats.held = s.stats.held.saturating_add(1);
                }
            }
            s.passing = s.passing.saturating_add(1);
        }
        // Counted out even if the submission panics (wgpu's default error handler does).
        struct Passed<'a>(&'a TrafficLight);
        impl Drop for Passed<'_> {
            fn drop(&mut self) {
                let mut s = self.0.lock();
                s.passing = s.passing.saturating_sub(1);
                drop(s);
                self.0.cv.notify_all();
            }
        }
        let _passed = Passed(self);
        submit()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc;

    #[test]
    fn green_light_submits_at_once() {
        let light = TrafficLight::new();
        assert_eq!(light.pass(|| 7), 7);
        assert_eq!(light.stats(), LightStats::default());
        assert!(!light.is_red());
    }

    /// Submissions on other threads wait while the light is red and go once it turns green.
    #[test]
    fn submissions_pause_while_red() {
        let light = TrafficLight::new();
        let red = light.red();
        assert!(light.is_red());
        let submitted = Arc::new(AtomicBool::new(false));
        let (l, s) = (light.clone(), submitted.clone());
        let worker = std::thread::spawn(move || l.pass(|| s.store(true, Ordering::SeqCst)));
        std::thread::sleep(Duration::from_millis(150));
        assert!(!submitted.load(Ordering::SeqCst), "a submission went through a red light");
        drop(red);
        worker.join().unwrap();
        assert!(submitted.load(Ordering::SeqCst));
        assert!(!light.is_red());
        assert_eq!(light.stats().held, 1);
        assert_eq!(light.stats().late, 0);
    }

    /// Turning red waits for a submission already under way (a configure must not overlap it).
    #[test]
    fn red_waits_for_submissions_under_way() {
        let light = TrafficLight::new();
        let (started_tx, started) = mpsc::channel();
        let (finish, finish_rx) = mpsc::channel::<()>();
        let done = Arc::new(AtomicBool::new(false));
        let (l, d) = (light.clone(), done.clone());
        let worker = std::thread::spawn(move || {
            l.pass(|| {
                started_tx.send(()).unwrap();
                finish_rx.recv().unwrap();
                std::thread::sleep(Duration::from_millis(50));
                d.store(true, Ordering::SeqCst);
            })
        });
        started.recv().unwrap();
        finish.send(()).unwrap();
        let red = light.red();
        assert!(done.load(Ordering::SeqCst), "the light turned red during a submission");
        drop(red);
        worker.join().unwrap();
        assert_eq!(light.stats().undrained, 0);
    }

    /// The thread holding the light red submits freely (no deadlock), and holds nest.
    #[test]
    fn holder_submits_and_holds_nest() {
        let light = TrafficLight::new();
        let outer = light.red();
        let inner = light.red();
        assert_eq!(light.pass(|| 1), 1);
        drop(inner);
        assert!(light.is_red(), "the outer hold still holds");
        drop(outer);
        assert!(!light.is_red());
        assert_eq!(light.stats().reds, 1);
        assert_eq!(light.stats().held, 0);
    }

    /// A light left red never blocks a submission for longer than its bound.
    #[test]
    fn a_stuck_red_light_lets_submissions_go_after_the_bound() {
        let light = Arc::new(TrafficLight::with_bounds(Duration::from_millis(100), Duration::from_millis(100)));
        let _red = light.red();
        let l = light.clone();
        let t = Instant::now();
        std::thread::spawn(move || l.pass(|| ())).join().unwrap();
        assert!(t.elapsed() >= Duration::from_millis(100));
        assert!(t.elapsed() < Duration::from_secs(5));
        assert_eq!(light.stats().late, 1);
    }

    /// A submission that never returns can't keep the light from turning red forever.
    #[test]
    fn turning_red_waits_at_most_its_bound() {
        let light = Arc::new(TrafficLight::with_bounds(Duration::from_millis(100), Duration::from_millis(100)));
        let (started_tx, started) = mpsc::channel();
        let (finish, finish_rx) = mpsc::channel::<()>();
        let l = light.clone();
        let worker = std::thread::spawn(move || {
            l.pass(|| {
                started_tx.send(()).unwrap();
                let _ = finish_rx.recv();
            })
        });
        started.recv().unwrap();
        let t = Instant::now();
        let red = light.red();
        assert!(t.elapsed() < Duration::from_secs(5));
        assert_eq!(light.stats().undrained, 1);
        drop(red);
        finish.send(()).unwrap();
        worker.join().unwrap();
    }

    /// A panicking submission is still counted out (the next red does not wait for it).
    #[test]
    fn a_panicking_submission_is_counted_out() {
        let light = TrafficLight::new();
        let l = light.clone();
        let _ = std::thread::spawn(move || l.pass(|| panic!("backend error"))).join();
        let t = Instant::now();
        drop(light.red());
        assert!(t.elapsed() < TrafficLight::MAX_DRAIN);
        assert_eq!(light.stats().undrained, 0);
    }
}
