//! GPU viewer frames read back without a frame worker waiting for them (native).
//!
//! The compositor runs on a device of its own (not egui's), so a native GPU frame comes back to
//! the CPU as RGBA8 bytes. A worker that composited a frame starts that readback with
//! [`Landing::begin`] and goes on to its next job, so the CPU work of the next frames overlaps
//! the GPU work of this one. One thread waits for the oldest readback's submission (readbacks
//! finish in submission order); the readbacks' map callbacks, run by whichever thread polls the
//! device, only hand the bytes over, and this thread finishes each frame with them ([`Land`]),
//! off the frame workers and the UI thread. It exits while nothing is in flight.
//!
//! Frames in flight are bounded by their bytes ([`BUDGET`]) and count ([`MAX_FLIGHTS`]); a frame
//! that does not fit is read back by its worker as before.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use effectcraft_gpu::wgpu::SubmissionIndex;
use effectcraft_gpu::{DisplayFrame, Gpu};

/// Finishes a frame with its premultiplied RGBA8 bytes (`None`: the readback failed).
pub(super) type Land = Box<dyn FnOnce(Option<Vec<u8>>) + Send>;

/// Readback bytes in flight at most: each frame holds a mapped staging buffer (host-visible
/// memory) and then its bytes until it lands. 32 HD frames, 7 UHD ones.
pub(super) const BUDGET: usize = 256 << 20;

/// Frames in flight at most.
pub(super) const MAX_FLIGHTS: usize = 32;

/// How often a frame whose submission was already waited for polls the device again (its map
/// callback only runs from a device poll).
const REPOLL: Duration = Duration::from_millis(5);

#[derive(Default)]
pub(super) struct Landing {
    state: Mutex<State>,
    cv: Condvar,
    /// Frames finished here (diagnostics, tests).
    landed: AtomicU64,
}

#[derive(Default)]
struct State {
    next: u64,
    flights: HashMap<u64, Flight>,
    /// Submissions copying frames in flight, oldest first.
    waiting: VecDeque<(u64, SubmissionIndex)>,
    /// Readbacks that came back (or failed), in arrival order.
    arrived: VecDeque<(u64, Option<Vec<u8>>)>,
    bytes: usize,
    /// The landing thread is running.
    thread: bool,
}

struct Flight {
    land: Land,
    bytes: usize,
    gpu: Gpu,
}

enum Step {
    Land(Land, Option<Vec<u8>>),
    Wait(Gpu, SubmissionIndex),
    Poll(Gpu),
}

impl Landing {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Start reading `frame` back; `land` runs on the landing thread with its bytes. Returns the
    /// flight's id (see [`Landing::cancel`]), or `land` back when the frame does not fit in flight
    /// (or the thread cannot start): its worker then reads it back itself.
    pub(super) fn begin(self: &Arc<Self>, gpu: &Gpu, frame: &DisplayFrame, land: Land) -> Result<u64, Land> {
        let Some(bytes) = (frame.width as usize).checked_mul(frame.height as usize).and_then(|n| n.checked_mul(4)) else { return Err(land) };
        let id = {
            let mut s = self.lock();
            if s.flights.len() >= MAX_FLIGHTS || (!s.flights.is_empty() && s.bytes.saturating_add(bytes) > BUDGET) {
                return Err(land);
            }
            if !s.thread {
                let me = self.clone();
                if std::thread::Builder::new().name("ec-frame-landing".into()).spawn(move || me.run()).is_err() {
                    return Err(land);
                }
                s.thread = true;
            }
            s.next = s.next.wrapping_add(1);
            let id = s.next;
            s.flights.insert(id, Flight { land, bytes, gpu: gpu.clone() });
            s.bytes = s.bytes.saturating_add(bytes);
            id
        };
        let me = Arc::downgrade(self);
        let start = || {
            gpu.read_display_start(frame, move |bytes| {
                if let Some(me) = me.upgrade() {
                    me.arrive(id, bytes);
                }
            })
        };
        let submission = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(start)) {
            Ok(submission) => submission,
            // The backend panicked: the caller takes the frame back unless it already landed.
            Err(_) => {
                return match self.take(id) {
                    Some(f) => Err(f.land),
                    None => Ok(id),
                };
            }
        };
        if let Some(submission) = submission {
            let mut s = self.lock();
            if s.flights.contains_key(&id) {
                s.waiting.push_back((id, submission));
            }
        }
        self.cv.notify_all();
        Ok(id)
    }

    /// Take a flight back before it lands: `true` when it had not landed (its readback is
    /// ignored and the caller finishes the frame), `false` when its [`Land`] owns the frame.
    pub(super) fn cancel(&self, id: u64) -> bool {
        // Dropped outside the lock (it holds the frame's job).
        self.take(id).is_some()
    }

    /// Remove a flight that has not landed.
    fn take(&self, id: u64) -> Option<Flight> {
        let mut s = self.lock();
        let flight = s.flights.remove(&id);
        if let Some(f) = &flight {
            s.bytes = s.bytes.saturating_sub(f.bytes);
        }
        flight
    }

    /// Frames finished here so far.
    #[cfg(test)]
    pub(super) fn landed(&self) -> u64 {
        self.landed.load(Ordering::Relaxed)
    }

    /// A readback came back (from the thread that polled the device): hand it to the thread.
    fn arrive(&self, id: u64, bytes: Option<Vec<u8>>) {
        {
            let mut s = self.lock();
            if s.flights.contains_key(&id) {
                s.arrived.push_back((id, bytes));
            }
        }
        self.cv.notify_all();
    }

    /// The landing thread: land arrived frames, else wait for the oldest submission in flight;
    /// exits once nothing is in flight.
    fn run(self: Arc<Self>) {
        loop {
            let step = {
                let mut s = self.lock();
                loop {
                    if let Some((id, bytes)) = s.arrived.pop_front() {
                        if let Some(f) = s.flights.remove(&id) {
                            s.bytes = s.bytes.saturating_sub(f.bytes);
                            break Step::Land(f.land, bytes);
                        }
                        continue;
                    }
                    if let Some((id, submission)) = s.waiting.pop_front() {
                        if let Some(f) = s.flights.get(&id) {
                            break Step::Wait(f.gpu.clone(), submission);
                        }
                        continue;
                    }
                    let Some(gpu) = s.flights.values().next().map(|f| f.gpu.clone()) else {
                        s.thread = false;
                        return;
                    };
                    // In flight, already waited for (or never submitted): its callback comes
                    // from a later poll, or the readback deadline settles it.
                    let (guard, wait) = self.cv.wait_timeout(s, REPOLL).unwrap_or_else(PoisonError::into_inner);
                    s = guard;
                    if wait.timed_out() && s.arrived.is_empty() && s.waiting.is_empty() {
                        break Step::Poll(gpu);
                    }
                }
            };
            match step {
                Step::Land(land, bytes) => {
                    self.landed.fetch_add(1, Ordering::Relaxed);
                    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| land(bytes))).is_err() {
                        log::error!("finishing a GPU frame panicked; that frame was skipped");
                    }
                }
                Step::Wait(gpu, submission) => {
                    // A failure (or the deadline) retires the context, which settles every
                    // readback in flight with `None`.
                    if let Err(e) = gpu.wait_submission(&submission) {
                        log::warn!("gpu frame readback: {e}");
                    }
                }
                Step::Poll(gpu) => gpu.poll(),
            }
        }
    }
}
