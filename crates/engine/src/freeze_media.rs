//! Bounded media verification. A stalled filesystem may retain one detached checker,
//! but cannot retain a freeze job or accumulate more blocked threads on each retry.

use std::sync::{
    Arc, OnceLock,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::time::Duration;

static BUSY: OnceLock<Arc<AtomicBool>> = OnceLock::new();

struct Permit(Arc<AtomicBool>);
impl Drop for Permit {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

fn bounded<T: Send + 'static>(busy: &Arc<AtomicBool>, timeout: Duration, work: impl FnOnce() -> Result<T, String> + Send + 'static) -> Result<T, String> {
    let began = web_time::Instant::now();
    while busy.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire).is_err() {
        if began.elapsed() >= timeout {
            return Err("Smart Freeze media verification is still waiting on the filesystem; originals remain available".into());
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    let permit = Permit(busy.clone());
    let (send, receive) = mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("freeze-media-check".into())
        .spawn(move || {
            let _permit = permit;
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(work))
                .unwrap_or_else(|_| Err("Smart Freeze media verification failed; originals remain available".into()));
            let _ = send.send(result);
        })
        .map_err(|e| format!("Cannot start Smart Freeze media verification: {e}"))?;
    receive
        .recv_timeout(timeout.saturating_sub(began.elapsed()))
        .map_err(|_| "Smart Freeze media verification timed out or stopped; check the drive and Refresh".to_string())?
}

pub fn stamps(paths: Vec<String>) -> Result<Vec<crate::freeze::MediaStamp>, String> {
    if paths.is_empty() {
        return Ok(vec![]);
    }
    let began = web_time::Instant::now();
    bounded(BUSY.get_or_init(|| Arc::new(AtomicBool::new(false))), Duration::from_secs(3), move || {
        paths
            .iter()
            .map(|path| {
                if began.elapsed() >= Duration::from_secs(3) {
                    return Err("Smart Freeze media verification exceeded three seconds; check the drive and Refresh".into());
                }
                crate::freeze::media_stamp(path)
            })
            .collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stalled_checker_times_out_without_accumulating_threads_and_recovers() {
        let busy = Arc::new(AtomicBool::new(false));
        let (release, blocked) = mpsc::sync_channel(1);
        let result = bounded(&busy, Duration::from_millis(25), move || {
            blocked.recv().map_err(|e| e.to_string())?;
            Ok(())
        });
        assert!(result.unwrap_err().contains("timed out"));
        assert!(busy.load(Ordering::Acquire));
        let ran = Arc::new(AtomicBool::new(false));
        let marker = ran.clone();
        assert!(
            bounded(&busy, Duration::from_millis(25), move || {
                marker.store(true, Ordering::Release);
                Ok(())
            })
            .is_err()
        );
        assert!(!ran.load(Ordering::Acquire), "retry must not spawn another blocked checker");
        release.send(()).unwrap();
        assert_eq!(bounded(&busy, Duration::from_secs(2), || Ok(42)).unwrap(), 42);
    }
}
