//! One detached native startup job. Dropping the receiver retires its result.
#![cfg(not(target_arch = "wasm32"))]

use std::sync::mpsc::{self, Receiver, TryRecvError};

pub(crate) struct Pending<T> {
    result: Receiver<Result<T, String>>,
}

impl<T: Send + 'static> Pending<T> {
    pub(crate) fn start(work: impl FnOnce() -> Result<T, String> + Send + 'static, wake: impl FnOnce() + Send + 'static) -> Result<Self, String> {
        let (send, result) = mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("effectcraft-gpu-startup".into())
            .spawn(move || {
                let value = std::panic::catch_unwind(std::panic::AssertUnwindSafe(work)).unwrap_or_else(|payload| {
                    let reason =
                        payload.downcast_ref::<String>().map(String::as_str).or_else(|| payload.downcast_ref::<&str>().copied()).unwrap_or("backend panicked");
                    Err(format!("GPU startup failed: {reason}"))
                });
                // A retired/closed viewer has dropped the receiver; never reinstall it.
                if send.send(value).is_ok() {
                    wake();
                }
            })
            .map_err(|e| format!("cannot start GPU initialization worker: {e}"))?;
        Ok(Self { result })
    }

    pub(crate) fn poll(&self) -> Option<Result<T, String>> {
        match self.result.try_recv() {
            Ok(result) => Some(result),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(Err("GPU startup worker disconnected".into())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pending_startup_does_not_wait_and_delivers_once() {
        let (send, wait) = mpsc::channel();
        let (wake, woke) = mpsc::channel();
        let pending = Pending::start(
            move || {
                wait.recv().unwrap();
                Ok(42)
            },
            move || {
                wake.send(()).unwrap();
            },
        )
        .unwrap();
        assert!(pending.poll().is_none());
        send.send(()).unwrap();
        woke.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        assert_eq!(pending.poll(), Some(Ok(42)));
    }
    #[test]
    fn retired_startup_never_wakes_or_installs_late_success() {
        let (send, wait) = mpsc::channel();
        let (wake, woke) = mpsc::channel();
        let pending = Pending::start(
            move || {
                wait.recv().unwrap();
                Ok(42)
            },
            move || {
                let _ = wake.send(());
            },
        )
        .unwrap();
        drop(pending);
        send.send(()).unwrap();
        assert!(woke.recv_timeout(std::time::Duration::from_secs(5)).is_err());
    }
    #[test]
    fn startup_failure_is_an_actionable_result() {
        let (wake, woke) = mpsc::channel();
        let pending = Pending::<()>::start(
            || Err("synthetic adapter failure".into()),
            move || {
                wake.send(()).unwrap();
            },
        )
        .unwrap();
        woke.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        assert_eq!(pending.poll(), Some(Err("synthetic adapter failure".into())));
    }
}
