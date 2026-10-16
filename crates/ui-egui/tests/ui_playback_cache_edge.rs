//! Playback that reaches the end of the cached frames waits for the next one and plays on by
//! itself once it renders: it never stays on the last cached frame.

use std::sync::{Arc, Mutex};

use effectcraft_engine::{
    Session,
    render::{NoFootage, Renderer},
};
use effectcraft_ui_egui::{
    EffectcraftApp,
    audio::{AudioDevice, AudioFeed},
    frames::{RemoteDone, RemoteFrame, RemoteFrames, RemoteJob, to_color_image},
};
use egui_kittest::Harness;
use serde_json::json;

/// Frames finish only when the test says so.
#[derive(Default)]
struct HeldRenderer(Mutex<Vec<(RemoteJob, RemoteDone)>>);

impl RemoteFrames for HeldRenderer {
    fn slots(&self) -> usize {
        4
    }
    fn start(&self, job: RemoteJob, done: RemoteDone) {
        self.0.lock().unwrap().push((job, done));
    }
}

impl HeldRenderer {
    fn finish_all(&self) -> usize {
        let jobs: Vec<_> = std::mem::take(&mut *self.0.lock().unwrap());
        let n = jobs.len();
        for (job, done) in jobs {
            let image = Renderer::new(&job.project, &NoFootage, job.opts).comp_frame(job.comp, job.t);
            let pixels = to_color_image(&image);
            done(Ok(RemoteFrame { width: image.width, height: image.height, rgba: pixels.pixels.iter().flat_map(|c| c.to_array()).collect(), ms: 20.0 }));
        }
        n
    }
}

/// An output the test plays by hand: one UI frame's worth of samples per step.
#[derive(Clone, Default)]
struct Output(Arc<Mutex<Option<Arc<AudioFeed>>>>);

impl AudioDevice for Output {
    fn sample_rate(&self) -> u32 {
        48_000
    }
    fn start(&mut self, feed: Arc<AudioFeed>) -> Result<(), String> {
        *self.0.lock().unwrap() = Some(feed);
        Ok(())
    }
    fn stop(&mut self) {}
}

fn frame(h: &Harness<'_, EffectcraftApp>) -> i64 {
    (h.state().session.time().seconds() * 30.0).round() as i64
}

#[test]
fn playback_waits_at_the_end_of_the_cached_frames_and_plays_on() {
    plays_on(false, true);
}

#[test]
fn playback_with_sound_waits_at_the_end_of_the_cached_frames_and_plays_on() {
    plays_on(true, true);
}

/// A machine without an audio output (a cloud desktop): the comp's sound can't play.
#[test]
fn playback_with_sound_but_no_output_plays_on() {
    plays_on(true, false);
}

fn plays_on(sound: bool, device: bool) {
    let mut session = Session::default();
    session.execute("comp.new", json!({"width": 64, "height": 36, "duration": 10, "frameRate": 30})).unwrap();
    let layer = session.execute("layer.newSolid", json!({"color": "#ffffff"})).unwrap()["layer"].as_u64().unwrap();
    if sound {
        session.execute("effect.apply", json!({"layers": [layer], "effect": "ec.audio.tone"})).unwrap();
        session.execute("layer.setSwitch", json!({"layers": [layer], "switch": "audio", "value": true})).unwrap();
    }
    let renderer = Arc::new(HeldRenderer::default());
    let mut app = EffectcraftApp::new(session);
    app.frames.set_remote(Some(renderer.clone()));
    let output = Output::default();
    let out = output.clone();
    app.hooks.audio_device = Some(Box::new(move |_| device.then(|| Box::new(out.clone()) as Box<dyn AudioDevice>)));
    let mut h = Harness::builder().with_size(egui::vec2(1400.0, 900.0)).with_step_dt(1.0 / 60.0).build_eframe(|_| app);
    h.run_steps(3);
    let now = h.ctx.input(|i| i.time);
    h.state_mut().play(now);
    let mut samples = vec![0.0f32; 1600];
    let mut step = |h: &mut Harness<'_, EffectcraftApp>, finish: bool| {
        h.state().frames.dispatch_remote();
        if finish {
            renderer.finish_all();
        }
        // The feeder mixes on its own thread; give it a moment, then play 1/60 s.
        std::thread::sleep(std::time::Duration::from_millis(2));
        if let Some(feed) = output.0.lock().unwrap().clone() {
            feed.pull(&mut samples);
        }
        h.step();
    };
    // Frames keep up for a second.
    for _ in 0..60 {
        step(&mut h, true);
    }
    let before = frame(&h);
    assert!(before > 10, "playing: frame {before}");
    // Rendering stalls for a second: playback reaches the end of the cached frames and waits.
    for _ in 0..60 {
        step(&mut h, false);
    }
    let held = frame(&h);
    assert!(h.state().playback.playing);
    // Then the frames render again: playback goes on without another Space.
    for _ in 0..60 {
        step(&mut h, true);
    }
    let after = frame(&h);
    assert!(h.state().playback.playing, "still playing");
    assert!(after > held + 10, "plays on once frames render (sound: {sound}): held at {held}, now {after}");
    if sound && device {
        assert!(output.0.lock().unwrap().is_some(), "the preview played its sound");
    }
}
