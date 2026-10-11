//! Bounded, like-for-like render samples. Compare the live composition on its configured
//! backend with the actual frozen-preview path, including cache read and compositing.

use effectcraft_project::freeze::{Sample, Section, Version};
use effectcraft_render::{Accelerator, ExprHost, FootageSource, RenderOpts, Renderer};

pub fn measure(
    project: &effectcraft_project::Project,
    section: &Section,
    version: &Version,
    footage: &dyn FootageSource,
    expr: Option<&dyn ExprHost>,
    accel: Option<&dyn Accelerator>,
    opts: RenderOpts,
    render_seconds: f64,
    ctl: &crate::jobs::TaskCtl,
) -> Option<Sample> {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let rate = version.frame_rate;
        let first = rate.frame_at(section.start);
        let last = rate.frame_at(section.end - effectcraft_time::Tick(1));
        let frames = std::collections::BTreeSet::from([first, first.saturating_add(last.saturating_sub(first) / 2), last]);
        let mut frozen_project = project.clone();
        let mut frozen_section = section.clone();
        frozen_section.cache = Some(version.clone());
        frozen_section.enabled = true;
        frozen_section.auto = false;
        frozen_project.freeze.sections = vec![frozen_section];
        let frozen = crate::freeze::Preview::for_comp(&frozen_project, section.target.comp());
        let mut live = Renderer::new(project, footage, opts);
        live.expr = expr;
        live.accel = accel;
        // This matches the viewer: a frozen selection composes on the CPU, while the live
        // baseline can use its GPU. Measuring only the selected layers would hide that cost.
        let mut cached = Renderer::new(&frozen_project, footage, opts);
        cached.expr = expr;
        cached.freeze = Some(&frozen);
        let (mut live_ms, mut cached_ms) = (0.0, 0.0);
        for frame in &frames {
            if ctl.cancelled() {
                return None;
            }
            let start = web_time::Instant::now();
            let image = effectcraft_render::passes::block_on(effectcraft_render::passes::comp_frame(&live, section.target.comp(), rate.tick_of(*frame)));
            std::hint::black_box(image);
            live_ms += start.elapsed().as_secs_f64() * 1000.0;
            let start = web_time::Instant::now();
            std::hint::black_box(cached.comp_frame(section.target.comp(), rate.tick_of(*frame)));
            cached_ms += start.elapsed().as_secs_f64() * 1000.0;
        }
        let count = frames.len() as u32;
        (count > 0).then(|| Sample { frames: count, live_ms: live_ms / f64::from(count), cached_ms: cached_ms / f64::from(count), render_seconds })
    }));
    result.ok().flatten()
}
