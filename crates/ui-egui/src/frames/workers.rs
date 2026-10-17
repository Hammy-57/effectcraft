//! The frame workers: plain threads, not a Rayon pool.
//!
//! A frame render waits for other threads: for a layer another frame is rendering (the layer
//! cache's single flight) and for a footage frame another thread is decoding. On a Rayon worker
//! such a wait has to run out quickly (250 ms, 1 s), because the thread it waits for may need
//! work queued behind the waiter; the waiter then renders or decodes the same thing again. With
//! every frame thread a Rayon worker, the frames of a new section stampeded: eight threads
//! waited, then all decoded the same PDF or rendered the same precomp, and a layer that renders
//! in 100 ms took 20 s (perf-3090, 17 Oct 2026). Frames now render on these threads and their
//! parallel pixel loops on Rayon's global pool, as the command-line renderer does: a frame
//! thread waits for the owner without holding up anyone's parallel work.

use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};

type Task = Box<dyn FnOnce() + Send>;

pub(crate) struct Workers {
    tx: Sender<Task>,
    threads: usize,
}

impl Workers {
    /// `threads` named `ec-frame-{i}` (`None` if the OS starts none of them).
    pub(crate) fn new(threads: usize) -> Option<Workers> {
        let (tx, rx) = channel::<Task>();
        let rx = Arc::new(Mutex::new(rx));
        let started = (0..threads.max(1))
            .filter(|i| {
                let rx = rx.clone();
                std::thread::Builder::new().name(format!("ec-frame-{i}")).spawn(move || run(&rx)).is_ok()
            })
            .count();
        (started > 0).then_some(Workers { tx, threads: started })
    }

    /// Run `task` on the next free worker.
    pub(crate) fn spawn(&self, task: impl FnOnce() + Send + 'static) {
        if self.tx.send(Box::new(task)).is_err() {
            log::error!("the frame workers stopped; a frame render was dropped");
        }
    }

    pub(crate) fn current_num_threads(&self) -> usize {
        self.threads
    }
}

/// A worker's loop: tasks until every sender is gone. A panicking task loses only itself.
fn run(rx: &Mutex<Receiver<Task>>) {
    loop {
        let task = match rx.lock() {
            Ok(rx) => rx.recv(),
            Err(_) => return,
        };
        let Ok(task) = task else { return };
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(task)).is_err() {
            log::error!("a frame render panicked; that frame was skipped");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// Tasks run on the named threads, a panic loses only its task, and a task waiting for
    /// another does not hold up a parallel loop (it runs on Rayon's global pool).
    #[test]
    fn workers_run_tasks_survive_panics_and_leave_parallel_loops_to_rayon() {
        let w = Workers::new(2).expect("threads");
        assert_eq!(w.current_num_threads(), 2);
        let (tx, rx) = channel();
        let t = tx.clone();
        w.spawn(move || t.send(std::thread::current().name().map(str::to_string)).unwrap());
        assert!(rx.recv_timeout(Duration::from_secs(10)).unwrap().unwrap().starts_with("ec-frame-"));
        w.spawn(|| panic!("synthetic frame render failure"));
        let t = tx.clone();
        w.spawn(move || t.send(Some(format!("{}", rayon::current_thread_index().is_none()))).unwrap());
        assert_eq!(rx.recv_timeout(Duration::from_secs(10)).unwrap().as_deref(), Some("true"), "not a Rayon worker");
        // One worker blocks until a parallel loop started on the other finishes.
        let (gate, open) = channel::<()>();
        w.spawn(move || {
            let _ = open.recv_timeout(Duration::from_secs(10));
        });
        let t = tx.clone();
        w.spawn(move || {
            use rayon::prelude::*;
            let n: u64 = (0..1000u64).into_par_iter().sum();
            t.send(Some(n.to_string())).unwrap();
        });
        assert_eq!(rx.recv_timeout(Duration::from_secs(10)).unwrap().as_deref(), Some("499500"));
        gate.send(()).unwrap();
    }
}
