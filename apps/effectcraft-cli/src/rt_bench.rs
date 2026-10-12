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
    let drag = args.flag("--rt-drag");
    let start = args.num("--time")?.unwrap_or(0.0);
    if !start.is_finite() || start < 0.0 {
        return Err(Failure::Error("--time must be finite and nonnegative".into()));
    }
    let init_start = std::time::Instant::now();
    let gpu = effectcraft_gpu::Gpu::try_headless().map_err(Failure::Error)?;
    // As the app: unified memory sizes its texture pool by RAM.
    gpu.context().set_system_memory(effectcraft_engine::sysinfo::memory().map_or(0, |m| m.total));
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
            // A drag changes only one transform at fixed composition time. Clone the
            // project snapshot just as an edit does; preserve the shared layer cache.
            let mut edited;
            let mut edited_renderer;
            let renderer = if drag {
                edited = project.clone();
                let layer = edited.comp_mut(cid).and_then(|c| c.layers.first_mut()).ok_or_else(|| Failure::Error("drag benchmark needs a layer".into()))?;
                let position = layer.props.prop_mut("transform/position").ok_or_else(|| Failure::Error("drag benchmark layer has no position".into()))?;
                let effectcraft_engine::keyframe::Value::Vec3(mut value) = position.value.clone() else {
                    return Err(Failure::Error("drag benchmark position is not Vec3".into()));
                };
                if let Some(x) = value.first_mut() {
                    *x += (offset % 20) as f64;
                }
                position.value = effectcraft_engine::keyframe::Value::Vec3(value);
                position.keys.clear();
                edited_renderer = Renderer::new(&edited, s.footage.as_ref(), RenderOpts { scale, backend: Backend::Gpu, ..Default::default() });
                edited_renderer.cache = Some(&cache);
                edited_renderer.accel = Some(&gpu);
                edited_renderer.expr = s.expr.as_deref();
                &edited_renderer
            } else {
                &r
            };
            let frame = if drag { base_frame } else { base_frame.saturating_add(offset) };
            gpu.render_display(renderer, cid, comp.frame_rate.tick_of(frame))
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
        let pool_before = gpu.context().pool_stats();
        let alloc_before = gpu.context().alloc_stats();
        let _ = effectcraft_gpu::take_pool_miss_sites();
        let mut samples = Vec::with_capacity(n);
        // Per frame in playback order: slow stretches start at cuts in the comp, which the
        // sorted samples hide.
        let mut frames = Vec::with_capacity(n);
        for i in 0..n {
            let (t0, p0, c0) = (gpu.context().transfer_stats(), gpu.context().pool_stats(), cache.stats());
            let a0 = gpu.context().alloc_stats();
            let now = std::time::Instant::now();
            render(i.saturating_add(1) as i64)?;
            let ms = now.elapsed().as_secs_f64() * 1000.0;
            let (t1, p1) = (gpu.context().transfer_stats(), gpu.context().pool_stats());
            let a1 = gpu.context().alloc_stats();
            let (c1, held) = (cache.stats(), cache.retained_stats());
            samples.push(ms);
            frames.push(json!({"offset": i.saturating_add(1), "ms": ms, "uploads": t1.uploads.saturating_sub(t0.uploads),
                "uploadBytes": t1.upload_bytes.saturating_sub(t0.upload_bytes), "readbackBytes": t1.readback_bytes.saturating_sub(t0.readback_bytes),
                "poolMisses": p1.1.saturating_sub(p0.1), "allocations": a1.allocations.saturating_sub(a0.allocations),
                "allocBytes": a1.bytes.saturating_sub(a0.bytes), "cacheCpuEntries": c1.entries, "cacheGpuEntries": c1.gpu_entries,
                "cacheHits": c1.hits.saturating_sub(c0.hits), "cacheMisses": c1.misses.saturating_sub(c0.misses),
                "retainedBytes": held.live_bytes, "retainedDebtBytes": held.debt_bytes, "retainedRecords": held.records}));
        }
        let after = gpu.context().transfer_stats();
        let pool_after = gpu.context().pool_stats();
        let alloc_after = gpu.context().alloc_stats();
        let miss_sites: Vec<_> = effectcraft_gpu::take_pool_miss_sites()
            .into_iter()
            .take(40)
            .map(|(site, w, h, count)| json!({"site": site, "width": w, "height": h, "count": count}))
            .collect();
        samples.sort_by(f64::total_cmp);
        // Instrument one additional moving frame, outside the latency sample population
        // (`--profile-offset K` instruments offset K instead, e.g. one inside a slow stretch).
        let profiled = args.opt("--profile-offset").and_then(|v| v.parse::<i64>().ok()).unwrap_or(n.saturating_add(1) as i64);
        profile::start();
        let result = render(profiled);
        let (events, dropped) = profile::finish();
        result?;
        let mut stages: BTreeMap<String, (usize, f64)> = BTreeMap::new();
        for event in &events {
            let entry = stages.entry(format!("{}:{}", event.cat, event.name)).or_default();
            entry.0 += 1;
            entry.1 += event.dur / 1000.0;
        }
        // Compute passes only (as before RT10a); copies (timed where the device can write
        // timestamps between encoder commands) are reported on their own.
        let encoder_stamp = |name: &str| matches!(name, "copy" | "readback_copy");
        let pass_events = || events.iter().filter(|e| e.cat == "gpu" && !encoder_stamp(&e.name));
        let pass_start = pass_events().map(|e| e.ts).fold(f64::INFINITY, f64::min);
        let gpu_window_ms = pass_events().map(|e| e.ts + e.dur).fold(0.0, f64::max) / 1000.0;
        let gpu_pass_window_ms = if pass_start.is_finite() { gpu_window_ms - pass_start / 1000.0 } else { 0.0 };
        let gpu_pass_sum_ms = pass_events().map(|e| e.dur).sum::<f64>() / 1000.0;
        let gpu_all_window_ms = events.iter().filter(|e| e.cat == "gpu").map(|e| e.ts + e.dur).fold(0.0, f64::max) / 1000.0;
        let copy_ms = events.iter().filter(|e| e.cat == "gpu" && matches!(e.name.as_str(), "copy" | "readback_copy")).map(|e| e.dur).sum::<f64>() / 1000.0;
        let p50 = percentile(&samples, 50);
        let p95 = percentile(&samples, 95);
        note!("{name} {label}: cold={cold_ms:.2} ms, p50={p50:.2} ms, p95={p95:.2} ms ({n} GPU viewer frames; transform drag={drag})");
        for (stage, (calls, ms)) in &stages {
            note!("  {stage}: {ms:.3} ms, {calls} calls (diagnostic frame, inclusive)");
        }
        let timestamps = gpu.device().features().contains(effectcraft_gpu::wgpu::Features::TIMESTAMP_QUERY);
        if !timestamps {
            note!("  GPU timestamps unavailable on this adapter/device");
        }
        rows.push(json!({"comp": name, "resolution": label, "frames": n, "transformDrag": drag, "coldMs": cold_ms, "p50Ms": p50, "p95Ms": p95,
            "samplesMs": samples, "framesInOrder": frames, "bitDepth": format!("{:?}", project.settings.bit_depth), "stagesInclusiveMs": stages, "profiledOffset": profiled, "droppedTraceEvents": dropped,
            "yuvConversions": after.yuv_conversions.saturating_sub(before.yuv_conversions),
            "yuvInputBytes": after.yuv_bytes.saturating_sub(before.yuv_bytes), "timestampQueries": timestamps, "gpuDiagnosticWindowMs": gpu_window_ms,
            "gpuPassWindowMs": gpu_pass_window_ms, "gpuPassSumMs": gpu_pass_sum_ms, "gpuAllWindowMs": gpu_all_window_ms, "gpuCopyMs": copy_ms,
            "allocations": alloc_after.allocations.saturating_sub(alloc_before.allocations), "allocBytes": alloc_after.bytes.saturating_sub(alloc_before.bytes), "poolMissSites": miss_sites, "uploads": after.uploads.saturating_sub(before.uploads),
            "uploadBytes": after.upload_bytes.saturating_sub(before.upload_bytes),
            "poolHits": pool_after.0.saturating_sub(pool_before.0),
            "poolMisses": pool_after.1.saturating_sub(pool_before.1),
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
    let report = json!({"adapter": effectcraft_render::Accelerator::name(&gpu), "initializationMs": initialization_ms, "colorFusion": effectcraft_gpu::color_fusion_enabled(), "yuvTransport": effectcraft_gpu::yuv_transport_enabled(), "warmup": warmup, "warmupMs": warmup_ms, "staticPipelines": gpu.static_pipeline_count(), "path": "GPU viewer + completion wait (no window presentation)", "rows": rows});
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

#[cfg(test)]
mod yuv_tests {
    /// Native generated clips stay local, like the profiling fixtures. Run with
    /// EFFECTCRAFT_YUV_ORACLE_CLIP and --ignored; no licensed media is embedded.
    #[test]
    #[ignore = "requires a local generated video and native GPU"]
    fn decoded_native_yuv_matches_media_rows_full_and_half() {
        use effectcraft_engine::project::{BitDepth, Comp, ItemKind, LayerSource, Project, build};
        use effectcraft_engine::time::{FrameRate, Tick};
        use effectcraft_render::{Backend, FootageSource, RenderOpts, Renderer};
        let path = std::env::var("EFFECTCRAFT_YUV_ORACLE_CLIP").expect("set generated clip path");
        let footage = effectcraft_media::probe(&path).expect("probe generated clip");
        let pool = effectcraft_media::MediaPool::new();
        let gpu = effectcraft_gpu::Gpu::try_headless().expect("native GPU required");
        assert!(effectcraft_gpu::yuv_transport_enabled());
        for depth in [BitDepth::Bpc8, BitDepth::Bpc32] {
            let mut project = Project::default();
            project.settings.bit_depth = depth;
            let comp = Comp::new(footage.width, footage.height, FrameRate::FPS_30, Tick::from_seconds_f64(2.0));
            let cid = project.add_item("YUV oracle", effectcraft_engine::color::Label::Sandstone, None, ItemKind::Comp(comp.clone().into()));
            let fid = project.add_item("generated video", effectcraft_engine::color::Label::Lavender, None, ItemKind::Footage(footage.clone()));
            let layer = build::layer(&mut project, &comp, "footage", LayerSource::Footage { item: fid }, (footage.width, footage.height), None);
            project.comp_mut(cid).unwrap().layers.push(layer);
            let frame = pool.frame_pixels(fid, &footage, Tick::ZERO).expect("actual decoded frame");
            let native = frame.yuv().expect("actual decoder keeps planar samples");
            native.validate().expect("valid native planes");
            for scale in [1.0, 0.5, 0.25] {
                let opts = RenderOpts { backend: Backend::Cpu, scale, ..Default::default() };
                let cpu = Renderer::new(&project, &pool, opts).comp_frame_cpu(cid, Tick::ZERO);
                let mut renderer = Renderer::new(&project, &pool, RenderOpts { backend: Backend::Gpu, ..opts });
                renderer.accel = Some(&gpu);
                let before = gpu.context().transfer_stats();
                let actual = gpu.within_memory(|| gpu.render(&renderer, cid, Tick::ZERO)).expect("guarded GPU work").expect("GPU frame");
                let after = gpu.context().transfer_stats();
                assert_eq!(after.readbacks - before.readbacks, 1, "only final frame readback");
                assert_eq!(after.yuv_conversions - before.yuv_conversions, 1, "native plane path used");
                assert!(after.upload_bytes - before.upload_bytes <= native.validate().unwrap() as u64, "no expanded RGBA upload");
                let tolerance = if depth == BitDepth::Bpc8 { 1.0 / 255.0 + 1e-6 } else { 1e-3 };
                let mut maximum = 0.0f32;
                assert_eq!(cpu.data.len(), actual.data.len());
                for (i, (expected, actual)) in cpu.data.iter().zip(&actual.data).enumerate() {
                    for (a, b) in expected.iter().zip(actual) {
                        maximum = maximum.max((a - b).abs());
                        assert!(b.is_finite() && (a - b).abs() <= tolerance, "{path} {depth:?} scale{scale} pixel{i}: {expected:?} != {actual:?}");
                    }
                }
                eprintln!("actual media oracle {path} {depth:?} scale{scale}: max{maximum}, upload{} bytes", after.upload_bytes - before.upload_bytes);
            }
        }
    }
    #[test]
    #[ignore = "requires a local generated footage benchmark project and native GPU"]
    fn decoded_native_yuv_original_effect_chain_matches_cpu() {
        use effectcraft_engine::project::{BitDepth, Project};
        use effectcraft_engine::time::Tick;
        use effectcraft_render::{Backend, RenderOpts, Renderer};
        let path = std::env::var("EFFECTCRAFT_YUV_ORACLE_PROJECT").expect("set frozen project path");
        let mut project = Project::from_json(&std::fs::read_to_string(&path).expect("read project")).expect("project");
        let cid = *project.items.iter().find(|(_, i)| i.name == "footage-4k").expect("footage-4k comp").0;
        struct CpuPlanes<'a>(&'a effectcraft_media::MediaPool);
        struct HiddenFrame(effectcraft_render::SharedFrame);
        impl effectcraft_render::FramePixels for HiddenFrame {
            fn size(&self) -> (u32, u32) {
                self.0.size()
            }
            fn rows(&self, y: usize, out: &mut [[f32; 4]]) {
                self.0.rows(y, out);
            }
        }
        impl effectcraft_render::FootageSource for CpuPlanes<'_> {
            fn frame(
                &self,
                i: effectcraft_engine::project::ItemId,
                f: &effectcraft_engine::project::Footage,
                t: Tick,
            ) -> Option<std::sync::Arc<effectcraft_render::Image>> {
                self.0.frame(i, f, t)
            }
            fn frame_pixels(
                &self,
                i: effectcraft_engine::project::ItemId,
                f: &effectcraft_engine::project::Footage,
                t: Tick,
            ) -> Option<effectcraft_render::SharedFrame> {
                self.0.frame_pixels(i, f, t).map(|p| std::sync::Arc::new(HiddenFrame(p)) as effectcraft_render::SharedFrame)
            }
            fn preview_frame(
                &self,
                i: effectcraft_engine::project::ItemId,
                f: &effectcraft_engine::project::Footage,
                t: Tick,
                w: u32,
                h: u32,
                scale: f64,
            ) -> Option<effectcraft_render::FootageFrame> {
                self.0
                    .preview_frame(i, f, t, w, h, scale)
                    .map(|p| effectcraft_render::FootageFrame { px: std::sync::Arc::new(HiddenFrame(p.px)), preview: p.preview })
            }
        }
        let pool = effectcraft_media::MediaPool::new();
        let gpu = effectcraft_gpu::Gpu::try_headless().expect("native GPU required");
        let mut failed_cases = 0;
        for depth in [BitDepth::Bpc8, BitDepth::Bpc32] {
            for scale in [1.0, 0.5, 0.25, 0.37] {
                project.settings.bit_depth = depth;
                let opts = RenderOpts { backend: Backend::Cpu, scale, ..Default::default() };
                let time = Tick::from_seconds_f64(0.5);
                let cpu = Renderer::new(&project, &pool, opts).comp_frame_cpu(cid, time);
                let mut renderer = Renderer::new(&project, &pool, RenderOpts { backend: Backend::Gpu, ..opts });
                renderer.accel = Some(&gpu);
                let before = gpu.context().transfer_stats();
                let actual = gpu.within_memory(|| gpu.render(&renderer, cid, time)).expect("guarded GPU").expect("GPU frame");
                let after = gpu.context().transfer_stats();
                let hidden = CpuPlanes(&pool);
                let mut baseline_renderer = Renderer::new(&project, &hidden, RenderOpts { backend: Backend::Gpu, ..opts });
                baseline_renderer.accel = Some(&gpu);
                let baseline = gpu.within_memory(|| gpu.render(&baseline_renderer, cid, time)).expect("guarded baseline").expect("baseline frame");
                if effectcraft_gpu::yuv_transport_enabled() && scale != 0.37 {
                    assert!(after.yuv_conversions > before.yuv_conversions, "native footage path used");
                    assert_eq!(after.readbacks - before.readbacks, 1, "only final frame readback");
                } else {
                    assert_eq!(after.yuv_conversions - before.yuv_conversions, 0);
                }
                assert_eq!(cpu.data.len(), actual.data.len());
                let tolerance = if depth == BitDepth::Bpc8 { 1.0 / 255.0 + 1e-6 } else { 1e-3 };
                let mut maximum = 0.0f32;
                let mut over = 0usize;
                let mut changed = 0usize;
                let mut pair_max = 0.0f32;
                let mut new_material = 0usize;
                let mut worsened_material = 0usize;
                for ((a, b), old) in cpu.data.iter().zip(&actual.data).zip(&baseline.data) {
                    for ((aa, bb), oo) in a.iter().zip(b).zip(old) {
                        let error = (aa - bb).abs() / aa.abs().max(1.0);
                        maximum = maximum.max(error);
                        over += usize::from(!bb.is_finite() || error > tolerance);
                        changed += usize::from(bb.to_bits() != oo.to_bits());
                        let pair_error = (bb - oo).abs() / oo.abs().max(1.0);
                        pair_max = pair_max.max(pair_error);
                        assert!(pair_error <= tolerance, "native versus existing GPU tolerance");
                        assert!(bb.is_finite() && oo.is_finite());
                        let old_error = (aa - oo).abs() / aa.abs().max(1.0);
                        new_material += usize::from(error > tolerance && old_error <= tolerance);
                        worsened_material += usize::from(error > tolerance && error > old_error + 1e-6);
                    }
                }
                failed_cases += usize::from(new_material != 0 || worsened_material != 0 || (depth == BitDepth::Bpc32 && over != 0));
                eprintln!(
                    "original effect chain {path} {depth:?} scale{scale}: max{maximum}, over{over}, changed{changed}, pairMax{pair_max}, new{new_material}, worse{worsened_material}, upload{} bytes",
                    after.upload_bytes - before.upload_bytes
                );
            }
        }
        assert_eq!(failed_cases, 0, "transport adds no material CPU-oracle errors; float output remains strict");
    }
}
