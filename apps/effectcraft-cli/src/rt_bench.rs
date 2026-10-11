//! RT0: completed viewer frames, consecutive times, separate diagnostic trace.
use super::*;
use effectcraft_render::{Backend, profile};
use std::collections::BTreeMap;

fn percentile(sorted: &[f64], percent: usize) -> f64 {
    let rank = sorted.len().saturating_mul(percent).div_ceil(100).saturating_sub(1);
    sorted.get(rank).copied().unwrap_or(0.0)
}

pub(super) fn run(s: &Session, args: &Args) -> Result<(), Failure> {
    let n = args.opt("--n").unwrap_or("60").parse::<usize>().map_err(|_| Failure::Error("--n must be an integer in 1..=600".into()))?;
    if !(1..=600).contains(&n) {
        return Err(Failure::Error("--n must be in 1..=600".into()));
    }
    let start = args.num("--time")?.unwrap_or(0.0);
    if !start.is_finite() || start < 0.0 {
        return Err(Failure::Error("--time must be finite and nonnegative".into()));
    }
    let init_start = std::time::Instant::now();
    let gpu = effectcraft_gpu::Gpu::try_headless().map_err(Failure::Error)?;
    let initialization_ms = init_start.elapsed().as_secs_f64() * 1000.0;
    let warmup_start = std::time::Instant::now();
    let warmup = args.flag("--rt-warmup");
    if warmup {
        gpu.warm_pipelines().map_err(Failure::Error)?;
    }
    let warmup_ms = if warmup { warmup_start.elapsed().as_secs_f64() * 1000.0 } else { 0.0 };
    let mut project = (*s.project).clone();
    project.settings.gpu_acceleration = true;
    let cid = if args.flag("--adv3d") { adv3d_bench_comp(&mut project) } else { s.active_comp_id() }
        .ok_or_else(|| Failure::Error("no benchmark composition".into()))?;
    if args.flag("--adv3d") {
        use effectcraft_engine::keyframe::{Keyframe, Value};
        if let Some(c) = project.comp_mut(cid) {
            for layer in &mut c.layers {
                if matches!(layer.source, effectcraft_engine::project::LayerSource::Camera)
                    && let Some(p) = layer.props.prop_mut("transform/position")
                    && let Value::Vec3(from) = p.value.clone()
                {
                    let mut to = from;
                    if let Some(x) = to.first_mut() {
                        *x += 150.0;
                    }
                    p.keys = vec![Keyframe::new(Tick::ZERO, Value::Vec3(from)), Keyframe::new(Tick::from_seconds_f64(10.0), Value::Vec3(to))];
                }
            }
        }
    }
    let comp = project.comp(cid).ok_or_else(|| Failure::Error("missing composition".into()))?;
    let fps = comp.frame_rate.as_f64();
    if !fps.is_finite() || fps <= 0.0 {
        return Err(Failure::Error("invalid benchmark frame rate".into()));
    }
    let last_frame = comp.frame_rate.frame_at(Tick::from_seconds_f64(start)).saturating_add(n.saturating_add(1) as i64);
    if comp.frame_rate.tick_of(last_frame) >= comp.duration {
        return Err(Failure::Error("benchmark extends beyond the comp; reduce --n or --time".into()));
    }
    let name = project.item(cid).map(|i| i.name.as_str()).unwrap_or("benchmark");
    let mut rows = Vec::new();
    let mut trace = Vec::new();
    for (label, scale) in [("Full", 1.0), ("Half", 0.5)] {
        s.footage.purge();
        gpu.context().clear_uploads();
        let cache = LayerCache::default();
        let mut r = Renderer::new(&project, s.footage.as_ref(), RenderOpts { scale, backend: Backend::Gpu, ..Default::default() });
        r.cache = Some(&cache);
        r.accel = Some(&gpu);
        r.expr = s.expr.as_deref();
        let base_frame = comp.frame_rate.frame_at(Tick::from_seconds_f64(start));
        let render = |offset: i64| -> Result<(), Failure> {
            gpu.render_display(&r, cid, comp.frame_rate.tick_of(base_frame.saturating_add(offset)))
                .ok_or_else(|| Failure::Error("GPU viewer frame failed; refusing CPU fallback benchmark".into()))?;
            let _wait = profile::span("gpu_completion_wait");
            gpu.wait();
            gpu.context().check_health().map_err(Failure::Error)
        };
        // A cold first frame is reported separately. It includes source raster and uploads.
        let cold_start = std::time::Instant::now();
        render(0)?;
        let cold_ms = cold_start.elapsed().as_secs_f64() * 1000.0;
        let before = gpu.context().transfer_stats();
        let mut samples = Vec::with_capacity(n);
        for i in 0..n {
            let now = std::time::Instant::now();
            render(i.saturating_add(1) as i64)?;
            samples.push(now.elapsed().as_secs_f64() * 1000.0);
        }
        let after = gpu.context().transfer_stats();
        samples.sort_by(f64::total_cmp);
        // Instrument one additional moving frame, outside the latency sample population.
        profile::start();
        let result = render(n.saturating_add(1) as i64);
        let (events, dropped) = profile::finish();
        result?;
        let mut stages: BTreeMap<String, (usize, f64)> = BTreeMap::new();
        for event in &events {
            let entry = stages.entry(format!("{}:{}", event.cat, event.name)).or_default();
            entry.0 += 1;
            entry.1 += event.dur / 1000.0;
        }
        let gpu_window_ms = events.iter().filter(|e| e.cat == "gpu").map(|e| e.ts + e.dur).fold(0.0, f64::max) / 1000.0;
        let p50 = percentile(&samples, 50);
        let p95 = percentile(&samples, 95);
        note!("{name} {label}: cold={cold_ms:.2} ms, p50={p50:.2} ms, p95={p95:.2} ms ({n} consecutive GPU viewer frames)");
        for (stage, (calls, ms)) in &stages {
            note!("  {stage}: {ms:.3} ms, {calls} calls (diagnostic frame, inclusive)");
        }
        let timestamps = gpu.device().features().contains(effectcraft_gpu::wgpu::Features::TIMESTAMP_QUERY);
        if !timestamps {
            note!("  GPU timestamps unavailable on this adapter/device");
        }
        rows.push(json!({"comp": name, "resolution": label, "frames": n, "coldMs": cold_ms, "p50Ms": p50, "p95Ms": p95,
            "samplesMs": samples, "bitDepth": format!("{:?}", project.settings.bit_depth), "stagesInclusiveMs": stages, "droppedTraceEvents": dropped,
            "timestampQueries": timestamps, "gpuDiagnosticWindowMs": gpu_window_ms, "uploads": after.uploads.saturating_sub(before.uploads),
            "uploadBytes": after.upload_bytes.saturating_sub(before.upload_bytes),
            "readbackBytes": after.readback_bytes.saturating_sub(before.readback_bytes)}));
        trace.extend(events.into_iter().map(|mut e| {
            e.tid = format!("{label}:{}", e.tid);
            e
        }));
    }
    if let Some(path) = args.opt("--trace") {
        let bytes = serde_json::to_vec(&json!({"traceEvents": trace, "displayTimeUnit": "ms",
            "otherData": {"gpuTimeline": "GPU intervals normalized independently: not correlated to CPU timestamps", "cpuStages": "Inclusive; nested spans overlap"}}))
        .map_err(|e| Failure::Error(e.to_string()))?;
        std::fs::write(path, bytes).map_err(|e| Failure::Error(format!("writing {path}: {e}")))?;
    }
    let report = json!({"adapter": effectcraft_render::Accelerator::name(&gpu), "initializationMs": initialization_ms, "colorFusion": effectcraft_gpu::color_fusion_enabled(), "warmup": warmup, "warmupMs": warmup_ms, "staticPipelines": gpu.static_pipeline_count(), "path": "GPU viewer + completion wait (no window presentation)", "rows": rows});
    println!("{}", serde_json::to_string(&report).map_err(|e| Failure::Error(e.to_string()))?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn nearest_rank_percentiles_include_tail() {
        let values: Vec<_> = (1..=20).map(f64::from).collect();
        assert_eq!(percentile(&values, 50), 10.0);
        assert_eq!(percentile(&values, 95), 19.0);
        assert_eq!(percentile(&[42.0], 95), 42.0);
        assert_eq!(percentile(&[], 95), 0.0);
    }
}
