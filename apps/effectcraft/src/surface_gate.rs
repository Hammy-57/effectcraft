//! RT10e: the window's side of the traffic light (`effectcraft_ui_egui::zero_copy`).
//!
//! The GPU compositor shares the window's wgpu device so viewer frames are drawn without a
//! copy. `Surface::configure` waits for that device to go idle and fails if the compositor
//! submits meanwhile, and eframe configures surfaces inside its winit event handling: on a resize
//! or a scale change, when a live resize ends, when a frame comes back outdated or its surface
//! lost, and when a window opens. So this wraps eframe's winit application and holds the light
//! red for the whole handling of every event that may configure a surface:
//!
//! - window creation, resizes and scale changes, and every event for [`SETTLE`] after one (the
//!   end of a live resize and the first frames at the new size reconfigure too);
//! - the rest of an event in which a frame came back outdated or lost ([`Gate::surface_status`],
//!   called by egui-wgpu just before it reconfigures), and the [`SETTLE`] after it (a lost surface
//!   is recreated at the next frame).
//!
//! The UI's new immediate viewports (floating panels, the video preview window) hold it while
//! they open (`zero_copy::hold_for_new_viewport`). Ordinary frames and input don't touch the
//! light, so frame workers submit while the window waits for vsync. Every wait is bounded
//! (`TrafficLight::MAX_HOLD`, `MAX_DRAIN`): a light left red can't freeze anything.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use effectcraft_ui_egui::zero_copy::TrafficLight;
use eframe::UserEvent;
use eframe::egui_wgpu::SurfaceErrorAction;
use eframe::wgpu::CurrentSurfaceTexture;
use winit::application::ApplicationHandler;
use winit::event::{DeviceEvent, DeviceId, StartCause, WindowEvent};
use winit::event_loop::ActiveEventLoop;
use winit::window::WindowId;

/// How long after a resize, a scale change or a surface error every event holds the light red.
pub const SETTLE: Duration = Duration::from_millis(300);

/// The light and when the window last changed.
pub struct Gate {
    pub light: Arc<TrafficLight>,
    /// Every event until then holds the light red.
    settle_until: Mutex<Option<Instant>>,
    /// Holds taken in the middle of an event (by [`Gate::surface_status`]), released when it ends.
    latched: Mutex<Vec<effectcraft_ui_egui::zero_copy::Red>>,
}

impl Gate {
    pub fn new() -> Arc<Self> {
        Arc::new(Self { light: TrafficLight::new(), settle_until: Mutex::default(), latched: Mutex::default() })
    }

    fn settle(&self, now: Instant) {
        *self.settle_until.lock().unwrap_or_else(PoisonError::into_inner) = Some(now + SETTLE);
    }

    fn settling(&self, now: Instant) -> bool {
        self.settle_until.lock().unwrap_or_else(PoisonError::into_inner).is_some_and(|t| now < t)
    }

    /// egui-wgpu's surface status hook (`WgpuConfiguration::on_surface_status`), wrapping its
    /// default `action`: a frame that comes back outdated or lost is reconfigured right after
    /// this returns (or recreated at the next frame), so hold the light red until this event ends.
    pub fn surface_status(&self, status: &CurrentSurfaceTexture, action: SurfaceErrorAction) -> SurfaceErrorAction {
        if matches!(action, SurfaceErrorAction::Reconfigure | SurfaceErrorAction::RecreateSurface) {
            log::debug!("surface {status:?}: holding the GPU compositor while it reconfigures");
            self.settle(Instant::now());
            let red = self.light.red();
            self.latched.lock().unwrap_or_else(PoisonError::into_inner).push(red);
        }
        action
    }

    /// Run one event's handling `f`, with the light red when it may configure a surface
    /// (`configures`) or while the window settles.
    fn dispatch<R>(&self, configures: bool, f: impl FnOnce() -> R) -> R {
        let now = Instant::now();
        if configures {
            self.settle(now);
        }
        let red = (configures || self.settling(now)).then(|| self.light.red());
        let out = f();
        // Holds latched during this event first, then this event's own.
        self.latched.lock().unwrap_or_else(PoisonError::into_inner).clear();
        drop(red);
        out
    }
}

/// Window events after which eframe configures a surface.
fn configures(event: &WindowEvent) -> bool {
    matches!(event, WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. })
}

/// eframe's winit application, with the light held red around surface configuration.
pub struct Gated<A> {
    pub app: A,
    pub gate: Arc<Gate>,
}

impl<A: ApplicationHandler<UserEvent>> ApplicationHandler<UserEvent> for Gated<A> {
    fn new_events(&mut self, event_loop: &ActiveEventLoop, cause: StartCause) {
        self.gate.dispatch(false, || self.app.new_events(event_loop, cause));
    }

    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        // Creates (or recreates) the window and its surface.
        self.gate.dispatch(true, || self.app.resumed(event_loop));
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        self.gate.dispatch(false, || self.app.user_event(event_loop, event));
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, window_id: WindowId, event: WindowEvent) {
        let configures = configures(&event);
        self.gate.dispatch(configures, || self.app.window_event(event_loop, window_id, event));
    }

    fn device_event(&mut self, event_loop: &ActiveEventLoop, device_id: DeviceId, event: DeviceEvent) {
        self.gate.dispatch(false, || self.app.device_event(event_loop, device_id, event));
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        self.gate.dispatch(false, || self.app.about_to_wait(event_loop));
    }

    fn suspended(&mut self, event_loop: &ActiveEventLoop) {
        self.gate.dispatch(true, || self.app.suspended(event_loop));
    }

    fn exiting(&mut self, event_loop: &ActiveEventLoop) {
        self.gate.dispatch(false, || self.app.exiting(event_loop));
    }

    fn memory_warning(&mut self, event_loop: &ActiveEventLoop) {
        self.gate.dispatch(false, || self.app.memory_warning(event_loop));
    }
}

/// `eframe::run_native` with the window's events gated ([`Gated`]). eframe's own runner keeps
/// the app-creation error it exits with; this one reports the last error logged instead.
pub fn run<'a>(name: &str, mut options: eframe::NativeOptions, creator: eframe::AppCreator<'a>, gate: Arc<Gate>) -> eframe::Result {
    use winit::platform::run_on_demand::EventLoopExtRunOnDemand as _;
    let mut builder = winit::event_loop::EventLoop::<UserEvent>::with_user_event();
    if let Some(hook) = options.event_loop_builder.take() {
        hook(&mut builder);
    }
    let mut event_loop = builder.build().map_err(eframe::Error::WinitEventLoop)?;
    let created = Arc::new(AtomicBool::new(false));
    let flag = created.clone();
    let creator: eframe::AppCreator<'a> = Box::new(move |cc| {
        flag.store(true, Ordering::Release);
        creator(cc)
    });
    let app = eframe::create_native(name, options, creator, &event_loop);
    let mut gated = Gated { app, gate };
    event_loop.run_app_on_demand(&mut gated).map_err(eframe::Error::WinitEventLoop)?;
    if created.load(Ordering::Acquire) {
        return Ok(());
    }
    let why = effectcraft_engine::logging::recent()
        .into_iter()
        .rev()
        .find(|l| l.contains("error") || l.contains("Error"))
        .unwrap_or_else(|| "the window's graphics could not be set up".into());
    Err(eframe::Error::AppCreation(why.into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resizes_and_scale_changes_configure() {
        use winit::dpi::PhysicalSize;
        assert!(configures(&WindowEvent::Resized(PhysicalSize::new(10, 10))));
        assert!(!configures(&WindowEvent::Occluded(false)));
        assert!(!configures(&WindowEvent::RedrawRequested));
        assert!(!configures(&WindowEvent::Focused(true)));
    }

    /// The light is red for a configuring event and the settle time after it, green otherwise.
    #[test]
    fn red_while_configuring_and_settling() {
        let gate = Gate::new();
        gate.dispatch(false, || assert!(!gate.light.is_red(), "an ordinary event leaves the light green"));
        gate.dispatch(true, || assert!(gate.light.is_red()));
        assert!(!gate.light.is_red(), "green between events");
        gate.dispatch(false, || assert!(gate.light.is_red(), "red while the window settles"));
        *gate.settle_until.lock().unwrap() = Some(Instant::now());
        gate.dispatch(false, || assert!(!gate.light.is_red()));
    }

    /// A surface error in the middle of an event turns the light red until that event ends.
    #[test]
    fn a_surface_error_holds_the_rest_of_the_event() {
        let gate = Gate::new();
        gate.dispatch(false, || {
            assert!(!gate.light.is_red());
            let action = gate.surface_status(&CurrentSurfaceTexture::Outdated, SurfaceErrorAction::Reconfigure);
            assert!(matches!(action, SurfaceErrorAction::Reconfigure));
            assert!(gate.light.is_red());
        });
        assert!(!gate.light.is_red());
        // A skipped frame (occluded window) reconfigures nothing.
        *gate.settle_until.lock().unwrap() = None;
        gate.dispatch(false, || {
            gate.surface_status(&CurrentSurfaceTexture::Occluded, SurfaceErrorAction::SkipFrame);
            assert!(!gate.light.is_red());
        });
    }
}
