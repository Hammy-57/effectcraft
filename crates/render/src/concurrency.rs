//! How many frames render at once (CR1): the viewer's frame pool and an export's batches.
//!
//! By default one frame per core, between 2 and 16. `EFFECTCRAFT_FRAME_THREADS` (or the CLI's
//! `--frame-threads N`, [`set_frame_threads`]) caps it: on a machine with many cores and one GPU,
//! sixteen GPU frames at once can exhaust video memory.

use std::sync::atomic::{AtomicUsize, Ordering};

/// The environment variable that caps [`frame_threads`].
pub const FRAME_THREADS_ENV: &str = "EFFECTCRAFT_FRAME_THREADS";

/// Most frame threads a cap may ask for.
const MAX_FRAME_THREADS: usize = 256;

/// Set by [`set_frame_threads`] (0: not set).
static OVERRIDE: AtomicUsize = AtomicUsize::new(0);

/// Cap [`frame_threads`] at `n` for this process (the CLI's `--frame-threads`), over
/// [`FRAME_THREADS_ENV`]. Frame pools made earlier keep their size.
pub fn set_frame_threads(n: usize) {
    OVERRIDE.store(n.min(MAX_FRAME_THREADS), Ordering::Relaxed);
}

/// Frames that may render at once on a machine with `parallelism` cores: `parallelism` between 2
/// and 16, capped by [`set_frame_threads`] or [`FRAME_THREADS_ENV`] (at least 1).
pub fn frame_threads(parallelism: usize) -> usize {
    let default = parallelism.clamp(2, 16);
    match OVERRIDE.load(Ordering::Relaxed) {
        0 => cap(default, std::env::var(FRAME_THREADS_ENV).ok().as_deref()),
        n => default.min(n).max(1),
    }
}

/// `default` capped by an [`FRAME_THREADS_ENV`] value: a whole number of at least 1, else (with a
/// warning) no cap.
fn cap(default: usize, var: Option<&str>) -> usize {
    let Some(v) = var else { return default };
    match v.trim().parse::<usize>() {
        Ok(n) if n >= 1 => default.min(n.min(MAX_FRAME_THREADS)),
        _ => {
            log::warn!("{FRAME_THREADS_ENV}={v:?} is not a whole number of frames of at least 1: using {default}");
            default
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_env_value_caps_the_default() {
        assert_eq!(cap(16, None), 16);
        assert_eq!(cap(16, Some("8")), 8);
        assert_eq!(cap(16, Some(" 4 ")), 4);
        assert_eq!(cap(4, Some("12")), 4, "a cap never raises the default");
        assert_eq!(cap(16, Some("0")), 16, "0 is no cap");
        assert_eq!(cap(16, Some("lots")), 16);
        assert_eq!(cap(16, Some("-3")), 16);
    }

    #[test]
    fn the_cli_cap_applies_to_the_per_core_default() {
        set_frame_threads(3);
        assert_eq!(frame_threads(64), 3);
        assert_eq!(frame_threads(1), 2, "under the cap: the default");
        set_frame_threads(0);
        if std::env::var_os(FRAME_THREADS_ENV).is_none() {
            assert_eq!(frame_threads(64), 16);
            assert_eq!(frame_threads(1), 2);
        }
    }
}
