//! Optional bounded Chrome trace recorder. Disabled builds inline spans to no-ops.
//! CPU spans are inclusive; do not add nested stage totals to frame wall time.

#[cfg(feature = "profile")]
mod enabled {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Mutex, OnceLock};
    use web_time::Instant;

    const LIMIT: usize = 100_000;
    static ACTIVE: AtomicBool = AtomicBool::new(false);
    #[derive(Clone, serde::Serialize)]
    pub struct Event {
        pub name: String,
        pub cat: &'static str,
        pub ts: f64,
        pub dur: f64,
        pub tid: String,
        pub pid: u32,
        pub ph: &'static str,
    }
    struct State {
        origin: Instant,
        active: bool,
        generation: u64,
        events: Vec<Event>,
        dropped: u64,
    }
    fn state() -> &'static Mutex<State> {
        static STATE: OnceLock<Mutex<State>> = OnceLock::new();
        STATE.get_or_init(|| Mutex::new(State { origin: Instant::now(), active: false, generation: 0, events: Vec::new(), dropped: 0 }))
    }
    pub struct Span(Option<(&'static str, Instant, u64)>);
    pub fn span(name: &'static str) -> Span {
        if !ACTIVE.load(Ordering::Relaxed) {
            return Span(None);
        }
        let s = state().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        Span(s.active.then(|| (name, Instant::now(), s.generation)))
    }
    fn push(s: &mut State, event: Event) {
        if s.events.len() < LIMIT && s.events.try_reserve(1).is_ok() {
            s.events.push(event);
        } else {
            s.dropped = s.dropped.saturating_add(1);
        }
    }
    impl Drop for Span {
        fn drop(&mut self) {
            let Some((name, start, generation)) = self.0 else { return };
            let end = Instant::now();
            let mut s = state().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if s.active && generation == s.generation {
                let ts = start.saturating_duration_since(s.origin).as_secs_f64() * 1e6;
                push(
                    &mut s,
                    Event {
                        name: name.into(),
                        cat: "cpu",
                        ts,
                        dur: end.saturating_duration_since(start).as_secs_f64() * 1e6,
                        tid: format!("{:?}", std::thread::current().id()),
                        pid: 1,
                        ph: "X",
                    },
                );
            }
        }
    }
    /// Start an isolated recording; spans from an older recording cannot leak into it.
    pub fn start() {
        let mut s = state().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        s.generation = s.generation.wrapping_add(1);
        s.origin = Instant::now();
        s.events.clear();
        s.dropped = 0;
        s.active = true;
        ACTIVE.store(true, Ordering::Relaxed);
    }
    /// Token for asynchronous GPU batches. No CPU/GPU clock correlation is claimed.
    pub fn token() -> Option<u64> {
        if !ACTIVE.load(Ordering::Relaxed) {
            return None;
        }
        let s = state().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        s.active.then_some(s.generation)
    }
    /// GPU intervals use their own clock; finish normalizes its origin independently.
    pub fn gpu(generation: u64, name: &str, micros: f64, start_micros: f64) {
        let mut s = state().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        record_gpu(&mut s, generation, name, micros, start_micros);
    }
    fn record_gpu(s: &mut State, generation: u64, name: &str, micros: f64, start_micros: f64) {
        if !s.active || s.generation != generation || !micros.is_finite() || micros < 0.0 || !start_micros.is_finite() || start_micros < 0.0 {
            return;
        }
        push(s, Event { name: name.into(), cat: "gpu", ts: start_micros, dur: micros, tid: "GPU (independent clock)".into(), pid: 2, ph: "X" });
    }
    /// An optional GPU diagnostic could not be collected; never silently report a full trace.
    pub fn lost(generation: u64) {
        let mut s = state().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if s.active && s.generation == generation {
            s.dropped = s.dropped.saturating_add(1);
        }
    }
    pub fn finish() -> (Vec<Event>, u64) {
        let mut s = state().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        s.active = false;
        ACTIVE.store(false, Ordering::Relaxed);
        let mut events = std::mem::take(&mut s.events);
        let origin = events.iter().filter(|e| e.cat == "gpu").map(|e| e.ts).min_by(f64::total_cmp).unwrap_or(0.0);
        for e in events.iter_mut().filter(|e| e.cat == "gpu") {
            e.ts -= origin;
        }
        (events, s.dropped)
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn trace_generation_and_bound() {
            // Local state: other renderer tests may record spans concurrently.
            let mut s = State { origin: Instant::now(), active: true, generation: 2, events: Vec::new(), dropped: 0 };
            record_gpu(&mut s, 1, "old_gpu", 1.0, 100.0);
            record_gpu(&mut s, 2, "fill", 12.0, 100.0);
            record_gpu(&mut s, 2, "invalid", f64::NAN, 100.0);
            assert_eq!(s.events.len(), 1);
            assert_eq!(s.events.first().unwrap().dur, 12.0);
            s.active = false;
            record_gpu(&mut s, 2, "stopped", 1.0, 100.0);
            assert_eq!(s.events.len(), 1);
            s.active = true;
            for _ in 0..LIMIT + 1 {
                record_gpu(&mut s, 2, "fill", 1.0, 100.0);
            }
            assert_eq!(s.events.len(), LIMIT);
            assert_eq!(s.dropped, 2);
        }
    }
}

#[cfg(feature = "profile")]
pub use enabled::*;

#[cfg(not(feature = "profile"))]
pub struct Span;
#[cfg(not(feature = "profile"))]
#[inline(always)]
pub fn span(_name: &'static str) -> Span {
    Span
}
