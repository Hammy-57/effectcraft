//! Bounded current-project/time fusion preparation. This is not a promise about
//! every sequence that a future edit or animation can introduce.
use effectcraft_project::{ItemId, LayerSource, Project};
use effectcraft_render::{Backend, NoFootage, RenderOpts, Renderer};
use effectcraft_time::Tick;

#[derive(Clone, Copy, Debug, Default)]
pub struct WarmupReport {
    pub layers: usize,
    pub ready_sequences: usize,
    pub declined_steps: usize,
    pub declined_scopes: usize,
    pub created: u64,
    pub compile_attempts: u64,
}

impl crate::Gpu {
    /// Prepare the selected snapshot without decoding footage or running effects.
    /// Expression-bearing comps are conservatively declined before draw_passes:
    /// expression hosts can sample pixels, including through other layers.
    pub fn warm_project(&self, project: &Project, comp: ItemId, time: Tick) -> WarmupReport {
        if self.context().check_health().is_err() {
            return WarmupReport { declined_scopes: 1, ..Default::default() };
        }
        let before = self.pipeline_count();
        let attempts = self.fusion_compile_attempts();
        let mut renderer = Renderer::new(project, &NoFootage, RenderOpts { backend: Backend::Gpu, ..Default::default() });
        renderer.accel = Some(self);
        let mut report = WarmupReport::default();
        visit(self, &renderer, comp, time, &mut Vec::new(), &mut 1024, &mut 4096, &mut report);
        report.created = self.pipeline_count().saturating_sub(before);
        report.compile_attempts = self.fusion_compile_attempts().saturating_sub(attempts);
        report
    }
}

#[allow(clippy::too_many_arguments)]
fn visit(
    gpu: &crate::Gpu,
    renderer: &Renderer,
    comp: ItemId,
    time: Tick,
    path: &mut Vec<ItemId>,
    layers: &mut usize,
    effects: &mut usize,
    report: &mut WarmupReport,
) {
    if gpu.context().check_health().is_err() || path.len() >= 8 || path.contains(&comp) || *layers == 0 || *effects == 0 {
        report.declined_scopes += 1;
        return;
    }
    let Some(ctx) = renderer.eval_ctx(comp, time) else {
        report.declined_scopes += 1;
        return;
    };
    // Bound work before draw_passes allocates visible lists or evaluates posterized time.
    if ctx.comp.layers.len() > *layers || ctx.comp.layers.iter().any(|l| !Renderer::effect_plan_safe(l, 4096)) {
        report.declined_scopes += 1;
        return;
    }
    *layers -= ctx.comp.layers.len();
    path.push(comp);
    for (ctx, visible) in renderer.draw_passes(&ctx) {
        for layer in visible {
            if *effects == 0 {
                report.declined_scopes += 1;
                break;
            }
            report.layers += 1;
            let count = layer.effects().map_or(0, |g| g.groups().take(257).count());
            if count > 256 || count > *effects {
                report.declined_scopes += 1;
                continue;
            }
            *effects -= count;
            if matches!(layer.source, LayerSource::Comp { .. }) {
                if let Some((sub, nested, at, _)) = renderer.precomp_source(&ctx, layer) {
                    visit(gpu, &sub, nested, at, path, layers, effects, report);
                } else {
                    report.declined_scopes += 1;
                }
            }
            if !renderer.visit_gpu_effect_runs(&ctx, layer, count, &mut |steps| {
                if gpu.context().check_health().is_err() {
                    report.declined_steps += steps.len();
                    return;
                }
                let (ready, declined) = crate::fusion::warm(&gpu.ctx, steps);
                report.ready_sequences += ready;
                report.declined_steps += declined;
            }) {
                report.declined_scopes += 1;
            }
        }
    }
    path.pop();
}
