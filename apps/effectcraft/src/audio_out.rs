//! Audio preview output through cpal (the device and channel mapping chosen in Settings ▸ Audio;
//! the default output device when none is set or it is gone).
//!
//! cpal streams are not `Send` on every platform, so each preview runs its stream on a small
//! dedicated thread that owns it until stopped. The callback pulls interleaved stereo from the
//! frontend's [`AudioFeed`] and maps it onto the device's channels (mono: L+R average; extra
//! channels: silence), converting to the device's sample format.

use std::collections::VecDeque;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SizedSample};
use effectcraft_ui_egui::audio::{AudioDevice, AudioFeed, AudioOutput};

/// Device callback period: about 10 ms, clamped so Pulse/PipeWire/ALSA does not open a
/// multi-hundred-millisecond default buffer that plays as silence at the start of preview (#554).
fn preferred_buffer_frames(rate: u32) -> u32 {
    (rate / 100).clamp(256, 2048)
}

/// Wait until the feeder has mixed a block, or until preview is stopped. Starting the stream
/// before that fills the device with silence, so the first real samples are heard late (#554).
fn wait_to_play(feed: &AudioFeed, stop: &mpsc::Receiver<()>) -> bool {
    loop {
        if feed.queued_frames() > 0 {
            return true;
        }
        match stop.recv_timeout(Duration::from_millis(2)) {
            Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => return false,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
    }
}

pub struct CpalOut {
    out: AudioOutput,
    rate: u32,
    clock: Arc<Mutex<AudibleClock>>,
    origin: Instant,
    stop: Option<mpsc::Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

/// A submitted buffer's real prefix (an underrun adds silence after it, which never advances
/// the composition clock). Times are relative to the native output's monotonic origin.
struct AudibleInterval {
    start: Duration,
    first: u64,
    real: u64,
    rate: u32,
}

struct AudibleClock {
    intervals: VecDeque<AudibleInterval>,
    last: u64,
}

const CLOCK_INTERVALS: usize = 256;

impl Default for AudibleClock {
    fn default() -> Self {
        Self { intervals: VecDeque::with_capacity(CLOCK_INTERVALS), last: 0 }
    }
}

impl AudibleClock {
    fn reset(&mut self) {
        self.intervals.clear();
        self.last = 0;
    }

    fn stop(&mut self, now: Duration) {
        self.position(now);
        self.intervals.clear();
    }

    fn record(&mut self, now: Duration, delay: Duration, first: u64, real: u64, rate: u32) {
        self.position(now);
        // Never allocate in the callback. If a broken driver queues excessive future intervals,
        // retain the audible history and hold at its end until another callback can be recorded.
        if self.intervals.len() < CLOCK_INTERVALS {
            let start = now.saturating_add(delay);
            self.intervals.push_back(AudibleInterval { start, first, real, rate });
        }
    }

    fn position(&mut self, now: Duration) -> u64 {
        while self.intervals.get(1).is_some_and(|next| next.start <= now) {
            self.intervals.pop_front();
        }
        if let Some(interval) = self.intervals.front().filter(|interval| interval.start <= now) {
            let elapsed = now.saturating_sub(interval.start).as_nanos();
            let samples = elapsed.saturating_mul(u128::from(interval.rate)) / 1_000_000_000;
            let played = samples.min(u128::from(interval.real)) as u64;
            // Latency estimates may change between callbacks; never rewind audible samples.
            self.last = self.last.max(interval.first.saturating_add(played));
        }
        self.last
    }
}

/// Names of the output devices (Settings ▸ Audio).
pub fn devices() -> Vec<String> {
    let host = cpal::default_host();
    #[allow(deprecated)]
    host.output_devices().map(|d| d.filter_map(|d| d.name().ok()).collect()).unwrap_or_default()
}

/// The named output device, or the default one.
fn device(name: &str) -> Option<cpal::Device> {
    let host = cpal::default_host();
    if !name.is_empty() {
        #[allow(deprecated)]
        let found = host.output_devices().ok().and_then(|mut d| d.find(|d| d.name().ok().as_deref() == Some(name)));
        if found.is_some() {
            return found;
        }
    }
    host.default_output_device()
}

/// Open the output device (`None` without one).
pub fn open(out: &AudioOutput) -> Option<Box<dyn AudioDevice>> {
    let dev = device(&out.device)?;
    let cfg = dev.default_output_config().ok()?;
    // Settings ▸ Audio ▸ Preview Sample Rate: open the device at that rate when it supports it
    // (otherwise the preview is mixed at that rate and resampled to the device's).
    let supports = |r: u32| {
        dev.supported_output_configs()
            .map(|mut c| c.any(|c| c.channels() == cfg.channels() && c.min_sample_rate().0 <= r && r <= c.max_sample_rate().0))
            .unwrap_or(false)
    };
    let rate = if out.rate > 0 && supports(out.rate) { out.rate } else { cfg.sample_rate().0 };
    Some(Box::new(CpalOut { out: out.clone(), rate, clock: Arc::new(Mutex::new(AudibleClock::default())), origin: Instant::now(), stop: None, thread: None }))
}

fn run<T: SizedSample + FromSample<f32>>(
    dev: &cpal::Device,
    cfg: &cpal::StreamConfig,
    feed: Arc<AudioFeed>,
    clock: Arc<Mutex<AudibleClock>>,
    origin: Instant,
    map: (usize, usize),
) -> Result<cpal::Stream, String> {
    let ch = cfg.channels as usize;
    // Output mapping: device channels (0-based) for left and right; out of range → 1 and 2.
    let (left, right) = if map.0 < ch && map.1 < ch { map } else { (0, 1) };
    let rate = cfg.sample_rate.0;
    let mut stereo: Vec<f32> = Vec::new();
    dev.build_output_stream(
        cfg,
        move |out: &mut [T], info: &cpal::OutputCallbackInfo| {
            let now = origin.elapsed();
            let frames = out.len() / ch.max(1);
            stereo.resize(frames * 2, 0.0);
            let first = feed.consumed();
            feed.pull(&mut stereo);
            let real = feed.consumed().saturating_sub(first);
            for (f, s) in out.chunks_mut(ch.max(1)).zip(stereo.as_chunks::<2>().0.iter()) {
                for (c, o) in f.iter_mut().enumerate() {
                    // Mono devices, or left and right mapped to the same channel, get the mix.
                    let v = if ch == 1 || (c == left && c == right) {
                        (s[0] + s[1]) * 0.5
                    } else if c == left {
                        s[0]
                    } else if c == right {
                        s[1]
                    } else {
                        0.0
                    };
                    *o = T::from_sample(v.clamp(-1.0, 1.0));
                }
            }
            // CPAL's playback timestamp is when this buffer's FIRST sample becomes audible.
            // Keep earlier intervals too: this callback may run while an older buffer plays.
            let ts = info.timestamp();
            let delay = ts.playback.duration_since(&ts.callback).unwrap_or_default();
            clock.lock().unwrap_or_else(std::sync::PoisonError::into_inner).record(now, delay, first, real, rate);
        },
        |e| log::warn!("audio output: {e}"),
        None,
    )
    .map_err(|e| e.to_string())
}

impl AudioDevice for CpalOut {
    fn sample_rate(&self) -> u32 {
        self.rate
    }

    fn start(&mut self, feed: Arc<AudioFeed>) -> Result<(), String> {
        self.stop();
        let (stop_tx, stop_rx) = mpsc::channel::<()>();
        let (ready_tx, ready_rx) = mpsc::channel::<Result<(), String>>();
        self.clock.lock().unwrap_or_else(std::sync::PoisonError::into_inner).reset();
        let clock = self.clock.clone();
        let origin = self.origin;
        let rate = self.rate;
        let name = self.out.device.clone();
        let map = ((self.out.left.max(1) - 1) as usize, (self.out.right.max(1) - 1) as usize);
        let thread = std::thread::Builder::new()
            .name("ec-audio-out".into())
            .spawn(move || {
                let Some(dev) = device(&name) else {
                    let _ = ready_tx.send(Err("no audio output device".into()));
                    return;
                };
                let sup = match dev.default_output_config() {
                    Ok(c) => c,
                    Err(e) => {
                        let _ = ready_tx.send(Err(e.to_string()));
                        return;
                    }
                };
                let mut cfg: cpal::StreamConfig = sup.config();
                cfg.sample_rate = cpal::SampleRate(rate);
                // A short period only where the default one is long (ALSA/PulseAudio on Linux, #554),
                // and only within what the device reports it takes.
                if cfg!(target_os = "linux") {
                    let want = preferred_buffer_frames(rate);
                    cfg.buffer_size = match sup.buffer_size() {
                        cpal::SupportedBufferSize::Range { min, max } if *min <= *max => cpal::BufferSize::Fixed(want.clamp(*min, *max)),
                        _ => cpal::BufferSize::Default,
                    };
                }
                let build = |cfg: &cpal::StreamConfig| match sup.sample_format() {
                    cpal::SampleFormat::F32 => run::<f32>(&dev, cfg, feed.clone(), clock.clone(), origin, map),
                    cpal::SampleFormat::I16 => run::<i16>(&dev, cfg, feed.clone(), clock.clone(), origin, map),
                    cpal::SampleFormat::U16 => run::<u16>(&dev, cfg, feed.clone(), clock.clone(), origin, map),
                    cpal::SampleFormat::I32 => run::<i32>(&dev, cfg, feed.clone(), clock.clone(), origin, map),
                    f => Err(format!("unsupported sample format {f:?}")),
                };
                let stream = match build(&cfg) {
                    Ok(s) => s,
                    Err(_) => {
                        // Some hosts reject a fixed period; a large default still plays, just later.
                        cfg.buffer_size = cpal::BufferSize::Default;
                        match build(&cfg) {
                            Ok(s) => s,
                            Err(e) => {
                                let _ = ready_tx.send(Err(e));
                                return;
                            }
                        }
                    }
                };
                let _ = ready_tx.send(Ok(()));
                if !wait_to_play(&feed, &stop_rx) {
                    return;
                }
                if let Err(e) = stream.play() {
                    log::warn!("audio output: {e}");
                    return;
                }
                let _ = stop_rx.recv();
                drop(stream);
            })
            .map_err(|e| e.to_string())?;
        self.stop = Some(stop_tx);
        self.thread = Some(thread);
        ready_rx.recv().map_err(|e| e.to_string())?
    }

    fn stop(&mut self) {
        if let Some(tx) = self.stop.take() {
            let _ = tx.send(());
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        self.clock.lock().unwrap_or_else(std::sync::PoisonError::into_inner).stop(self.origin.elapsed());
    }

    fn audible_frames(&self) -> Option<u64> {
        Some(self.clock.lock().unwrap_or_else(std::sync::PoisonError::into_inner).position(self.origin.elapsed()))
    }
}

impl Drop for CpalOut {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preferred_buffer_is_about_ten_milliseconds() {
        assert_eq!(preferred_buffer_frames(48_000), 480);
        assert_eq!(preferred_buffer_frames(8_000), 256);
        assert_eq!(preferred_buffer_frames(192_000), 1920);
    }

    #[test]
    fn wait_to_play_returns_once_the_feeder_has_samples() {
        let feed = Arc::new(AudioFeed::default());
        let (_tx, rx) = mpsc::channel();
        let f = feed.clone();
        let h = std::thread::spawn(move || wait_to_play(&f, &rx));
        assert_eq!(feed.queued_frames(), 0);
        std::thread::sleep(Duration::from_millis(20));
        feed.push(&[0.1, 0.2]);
        assert!(h.join().unwrap());
        assert_eq!(feed.queued_frames(), 1);
    }

    #[test]
    fn wait_to_play_stops_without_samples() {
        let feed = AudioFeed::default();
        let (tx, rx) = mpsc::channel();
        let h = std::thread::spawn(move || wait_to_play(&feed, &rx));
        tx.send(()).unwrap();
        assert!(!h.join().unwrap());
    }
}

#[cfg(test)]
mod clock_tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn cached_video_clock_advances_between_large_device_callbacks() {
        for frames in [2048, 4096] {
            let mut clock = AudibleClock::default();
            clock.record(Duration::ZERO, Duration::ZERO, 0, frames, 48_000);
            // No second device callback: a 30fps viewer can still show its second frame.
            assert_eq!(clock.position(ms(34)), 1632);
            assert!(clock.position(ms(34)) / 1600 == 1);
            assert_eq!(clock.position(ms(42)), 2016);
            // Never extrapolate beyond audio actually supplied to the device.
            assert_eq!(clock.position(ms(500)), frames);
        }
    }

    #[test]
    fn fully_cached_preview_has_no_callback_sized_frame_jumps() {
        for (numerator, denominator) in [(30u64, 1u64), (30_000, 1001)] {
            for rate in [44_100u64, 48_000] {
                for buffer in [2048u64, 4096] {
                    let mut clock = AudibleClock::default();
                    let mut next_sample = 0u64;
                    let mut last = 0u64;
                    let mut changed = 0;
                    for refresh in 0..=120u64 {
                        let now = Duration::from_nanos(refresh * 1_000_000_000 / 120 + 1000);
                        loop {
                            let callback = Duration::from_nanos(next_sample * 1_000_000_000 / rate);
                            if callback > now {
                                break;
                            }
                            clock.record(callback, Duration::ZERO, next_sample, buffer, rate as u32);
                            next_sample += buffer;
                        }
                        let frame = clock.position(now) * numerator / (rate * denominator);
                        assert!(frame <= last + 1, "{buffer}-sample callbacks at {rate} Hz skipped cached video: {last} -> {frame}");
                        if frame != last {
                            changed += 1;
                        }
                        last = frame;
                    }
                    assert_eq!(changed, numerator / denominator, "every cached frame due within one second can be shown");
                }
            }
        }
    }

    #[test]
    fn future_callbacks_keep_the_currently_audible_interval() {
        let mut clock = AudibleClock::default();
        // Three buffers submitted before the first is audible.
        clock.record(ms(0), ms(30), 0, 960, 48_000);
        clock.record(ms(10), ms(40), 960, 960, 48_000);
        clock.record(ms(20), ms(50), 1920, 960, 48_000);
        assert_eq!(clock.position(ms(25)), 0);
        assert_eq!(clock.position(ms(35)), 240);
        assert_eq!(clock.position(ms(45)), 720);
        assert_eq!(clock.position(ms(55)), 1200);
        assert_eq!(clock.position(ms(75)), 2160);
        assert_eq!(clock.position(ms(100)), 2880);
    }

    #[test]
    fn partial_and_complete_underruns_hold_after_the_real_prefix() {
        let mut clock = AudibleClock::default();
        clock.record(ms(0), ms(10), 0, 480, 48_000); // 10ms real audio, then silence
        clock.record(ms(1), ms(39), 480, 0, 48_000); // wholly silent buffer
        clock.record(ms(2), ms(68), 480, 960, 48_000); // audio resumes at 70ms
        assert_eq!(clock.position(ms(15)), 240);
        assert_eq!(clock.position(ms(30)), 480);
        assert_eq!(clock.position(ms(50)), 480);
        assert_eq!(clock.position(ms(69)), 480);
        assert_eq!(clock.position(ms(75)), 720);
        assert_eq!(clock.position(ms(1000)), 1440);
    }

    #[test]
    fn stop_seek_and_restart_do_not_reuse_a_previous_audio_cursor() {
        let mut clock = AudibleClock::default();
        clock.record(ms(0), ms(0), 0, 4800, 48_000);
        clock.stop(ms(20));
        assert_eq!(clock.position(ms(200)), 960, "stop holds the audible cursor");
        clock.reset();
        assert_eq!(clock.position(ms(200)), 0);
        clock.record(ms(200), ms(10), 0, 960, 48_000);
        assert_eq!(clock.position(ms(205)), 0);
        assert_eq!(clock.position(ms(215)), 240);
    }

    #[test]
    fn changing_latency_never_rewinds_and_future_history_is_bounded() {
        let mut clock = AudibleClock::default();
        clock.record(ms(0), ms(0), 0, 960, 48_000);
        assert_eq!(clock.position(ms(15)), 720);
        clock.record(ms(16), ms(40), 960, 960, 48_000);
        assert_eq!(clock.position(ms(40)), 960, "larger latency holds at the real prefix end");
        assert_eq!(clock.position(ms(60)), 1152);
        assert!(clock.position(ms(59)) >= 1152, "even a slightly older observation cannot rewind");
        clock.reset();
        let capacity = clock.intervals.capacity();
        for i in 0..1000 {
            clock.record(ms(0), ms(1000 + i), i * 48, 48, 48_000);
        }
        assert_eq!(clock.intervals.len(), CLOCK_INTERVALS);
        assert_eq!(clock.intervals.capacity(), capacity, "callback recording never grows the allocation");
        assert_eq!(clock.position(ms(500)), 0, "future buffers are not audible yet");
    }
}
