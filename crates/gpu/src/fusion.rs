//! Exact pointwise programs specialized by effect sequence; parameters remain dynamic.
use crate::context::{Enc, GpuContext, GpuImage, Params, init_resource};
use effectcraft_render::FxStep;
use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};

const MAX_STEPS: usize = 32;
const MAX_PROGRAMS: usize = 32;
const WORDS: usize = 64;
const MAX_DATA: usize = 1 << 20;
#[derive(Default)]
pub(crate) struct Pipelines {
    cache: Mutex<HashMap<Vec<u32>, Option<wgpu::ComputePipeline>>>,
    created: std::sync::atomic::AtomicU64,
    attempted: std::sync::atomic::AtomicU64,
}
impl Pipelines {
    pub(crate) fn attempted(&self) -> u64 {
        self.attempted.load(std::sync::atomic::Ordering::Relaxed)
    }
    pub(crate) fn created(&self) -> u64 {
        self.created.load(std::sync::atomic::Ordering::Relaxed)
    }
}

pub(crate) fn enabled() -> bool {
    #[cfg(feature = "profile")]
    {
        static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *ENABLED.get_or_init(|| std::env::var("EFFECTCRAFT_PROFILE_FUSION").as_deref() != Ok("0"))
    }
    #[cfg(not(feature = "profile"))]
    {
        true
    }
}
fn opcode(step: &FxStep) -> Option<u32> {
    if !effectcraft_effects::catalog::gpu_supported(step.spec.id, &step.ctx) {
        return None;
    }
    match step.spec.id {
        "ec.color.tint" => Some(1),
        "ec.color.levels" if effectcraft_effects::levels_channels_identity(&step.ctx) => Some(4),
        "ec.color.exposure" => Some(5),
        "ec.color.curves" if ["rgb", "red", "green", "blue", "alpha"].iter().all(|id| step.ctx.params.s(id).len() <= 65536) => Some(8),
        _ => None,
    }
}
fn program(steps: &[FxStep]) -> Option<(Vec<u32>, Vec<f32>)> {
    opcode(steps.first()?)?;
    opcode(steps.get(1)?)?;
    let ids: Vec<_> = steps.iter().take(MAX_STEPS).map_while(opcode).collect();
    if ids.len() < 2 {
        return None;
    }
    let mut data = vec![0.0; ids.len().checked_mul(WORDS)?];
    for (index, (step, id)) in steps.iter().zip(&ids).enumerate() {
        let mut p = if *id == 8 {
            let mut p = Params::default();
            p.u[0][0] = 8;
            let mut offsets = [-1.0; 5];
            for (i, name) in ["rgb", "red", "green", "blue", "alpha"].iter().enumerate() {
                if let Some(curve) = effectcraft_effects::Curve::parse(step.ctx.params.s(name)) {
                    let (xs, ys, ms, lut) = curve.tables();
                    let extra = 1usize.checked_add(xs.len())?.checked_add(ys.len())?.checked_add(ms.len())?.checked_add(lut.len())?;
                    if data.len().checked_add(extra)? > MAX_DATA {
                        return None;
                    }
                    *offsets.get_mut(i)? = data.len() as f32;
                    data.push(xs.len() as f32);
                    data.extend_from_slice(xs);
                    data.extend_from_slice(ys);
                    data.extend_from_slice(ms);
                    data.extend_from_slice(lut);
                }
            }
            if offsets.iter().all(|v| *v < 0.0) {
                return None;
            }
            p.f[0] = [offsets[0], offsets[1], offsets[2], offsets[3]];
            p.f[1][0] = offsets[4];
            p
        } else {
            crate::effects::pointwise_params(step.spec.id, &step.ctx, [0.0; 2], 1.0)?
        };
        p.u[0][0] = *id;
        let base = index.checked_mul(WORDS)?;
        let target = data.get_mut(base..base.checked_add(WORDS)?)?;
        // These four programs use only small integer opcodes/flags, exactly representable
        // as f32. Numeric packing avoids subnormal bit-pattern flushing on GPU drivers.
        for (dst, word) in target.iter_mut().zip(p.u.iter().flatten().map(|v| *v as f32).chain(p.f.iter().flatten().copied())) {
            *dst = word;
        }
    }
    Some((ids, data))
}
fn source(ids: &[u32]) -> Option<String> {
    if !(2..=MAX_STEPS).contains(&ids.len()) {
        return None;
    }
    let mut src = [include_str!("shaders/common.wgsl"), include_str!("shaders/kernels.wgsl")].concat();
    src.push_str(
        r#"
fn colour_f4(base: u32) -> vec4<f32> {
    return vec4<f32>(data[base], data[base+1u], data[base+2u], data[base+3u]);
}
@compute @workgroup_size(16,16)
fn colour_chain(@builtin(global_invocation_id) gid: vec3<u32>) {
    let p = vec2<i32>(gid.xy);
    let dims = out_dims();
    if (p.x >= dims.x || p.y >= dims.y) { return; }
    var px = textureLoad(src, p, 0);
"#,
    );
    // Emit compact effect-specific calls. Passing the full Params array through
    // the generic switch was much slower on Metal; keep specialization explicit.
    for (i, id) in ids.iter().enumerate() {
        let base = i * WORDS;
        let f0 = base + 16;
        let f1 = base + 20;
        let f2 = base + 24;
        let call = match id {
            1 => format!("colour_tint(px, colour_f4({f0}u).xyz, colour_f4({f1}u).xyz, data[{f2}u])"),
            4 => format!("colour_levels(px, vec2<u32>(colour_f4({base}u).yz), colour_f4({f0}u), data[{f1}u])"),
            5 => format!("colour_exposure(px, u32(data[{}u]), colour_f4({f0}u).xyz, colour_f4({f1}u).xyz, colour_f4({f2}u).xyz)", base + 1),
            8 => format!("colour_curves(px, colour_f4({f0}u), data[{f1}u])"),
            _ => return None,
        };
        src.push_str(&format!("    px = {call}; if (P.f[0].x > 0.0) {{ px = quantize_px(px, P.f[0].x, P.f[0].y); }}\n"));
    }
    src.push_str("    textureStore(out, p, px);\n}\n");
    Some(src)
}
impl Pipelines {
    fn get(&self, g: &GpuContext, ids: &[u32]) -> Option<wgpu::ComputePipeline> {
        let mut cache = self.cache.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(p) = cache.get(ids) {
            return p.clone();
        }
        // Bound compiled code and failure records; long-tail stacks keep the existing path.
        if cache.len() >= MAX_PROGRAMS {
            return None;
        }
        // Includes source/module/layout preparation, not just the backend API call.
        self.attempted.store(self.attempted().saturating_add(1), std::sync::atomic::Ordering::Relaxed);
        let compile = || -> Result<wgpu::ComputePipeline, String> {
            let source = source(ids).ok_or_else(|| "unsupported colour fusion opcode".to_string())?;
            let module = init_resource(&g.device, "colour chain module", || {
                g.device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("colour chain"), source: wgpu::ShaderSource::Wgsl(source.into()) })
            })?;
            let layout = init_resource(&g.device, "colour chain layout", || {
                g.device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some("colour chain"),
                    bind_group_layouts: &[Some(&g.bgl)],
                    immediate_size: 0,
                })
            })?;
            init_resource(&g.device, "colour chain pipeline", || {
                g.device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                    label: Some("colour chain"),
                    layout: Some(&layout),
                    module: &module,
                    entry_point: Some("colour_chain"),
                    compilation_options: Default::default(),
                    cache: None,
                })
            })
        };
        let result = compile().map_err(|error| log::warn!("GPU colour fusion unavailable; using separate passes: {error}")).ok();
        if result.is_some() {
            // Compilation is serialized by the cache mutex; readers only load this counter.
            self.created.store(self.created().saturating_add(1), std::sync::atomic::Ordering::Relaxed);
        }
        cache.insert(ids.to_vec(), result.clone());
        result
    }
}

/// Prepare the same prefixes used by apply_chain; failed program construction or
/// compilation advances one effect, allowing a later eligible suffix.
pub(crate) fn warm(g: &GpuContext, steps: &[FxStep]) -> (usize, usize) {
    if !enabled() {
        return (0, steps.len());
    }
    let mut remaining = steps;
    let (mut ready, mut declined) = (0, 0);
    while let Some((_, rest)) = remaining.split_first() {
        if let Some((ids, _)) = program(remaining)
            && g.fusion.get(g, &ids).is_some()
        {
            ready += 1;
            remaining = remaining.get(ids.len()..).unwrap_or_default();
        } else {
            declined += 1;
            remaining = rest;
        }
    }
    (ready, declined)
}

pub(crate) fn apply(e: &mut Enc, steps: &[FxStep], input: &GpuImage, levels: Option<f32>) -> Option<(GpuImage, usize)> {
    if !enabled() {
        return None;
    }
    let (ids, data) = program(steps)?;
    let pipeline = e.g.fusion.get(e.g, &ids)?;
    let buffer = e.data(&data);
    let mut p = Params::default();
    p.f[0] = [levels.unwrap_or(0.0), levels.map_or(0.0, |v| 1.0 / v), 0.0, 0.0];
    let mut out = e.scratch(input.width, input.height);
    e.dispatch_pipeline("colour_chain", &pipeline, &p, input, None, &out, Some(&buffer), (out.width.div_ceil(16), out.height.div_ceil(16)), None);
    out.levels = levels;
    Some((out, ids.len()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effects::{GBuf, apply_chain};
    use effectcraft_effects::{Buf, EffectCtx, EffectEnv, Params as FxParams};
    use effectcraft_keyframe::Value;

    const IDS: [&str; 4] = ["ec.color.levels", "ec.color.curves", "ec.color.tint", "ec.color.exposure"];
    fn params() -> Vec<FxParams> {
        let mut params: Vec<_> = IDS
            .iter()
            .map(|id| {
                let spec = effectcraft_effects::find(id).unwrap();
                FxParams { values: spec.params.iter().map(|p| (p.id.to_string(), effectcraft_effects::default_value(p, [17.0, 13.0]))).collect() }
            })
            .collect();
        params[0].values.insert("gamma".into(), Value::Scalar(1.2));
        params[0].values.insert("clipToOutputWhite".into(), Value::Enum(0));
        params[1].values.insert("rgb".into(), Value::Str("0,0 0.3,0.4 1,1".into()));
        params[1].values.insert("alpha".into(), Value::Str("0,0.1 0.5,0.6 1,1".into()));
        params[2].values.insert("amount".into(), Value::Scalar(35.0));
        params[3].values.insert("master/exposure".into(), Value::Scalar(0.3));
        params
    }
    fn steps(params: &[FxParams]) -> Vec<FxStep<'_>> {
        params
            .iter()
            .zip(IDS.iter().cycle())
            .map(|(p, id)| FxStep {
                spec: effectcraft_effects::find(id).unwrap(),
                ctx: EffectCtx { params: p, time: 0.0, layer_size: [17.0, 13.0], seed: 3, adjustment: false, env: EffectEnv::default() },
            })
            .collect()
    }
    fn assert_pixels(a: &effectcraft_raster::Image, b: &effectcraft_raster::Image, tolerance: f32) {
        assert_eq!((a.width, a.height), (b.width, b.height));
        for (index, (a, b)) in a.data.iter().zip(&b.data).enumerate() {
            for (channel, (a, b)) in a.iter().zip(b).enumerate() {
                assert!(a.is_finite() && b.is_finite() && (a - b).abs() <= tolerance, "pixel{index} channel{channel}: {a} vs {b}");
            }
        }
    }
    #[test]
    fn creation_count_survives_cache_retirement() {
        let pipelines = Pipelines::default();
        pipelines.created.store(33, std::sync::atomic::Ordering::Relaxed);
        let mut cache = pipelines.cache.lock().unwrap();
        for i in 0..MAX_PROGRAMS {
            cache.insert(vec![i as u32], None);
        }
        assert_eq!(pipelines.created(), 33);
        cache.clear();
        assert_eq!(pipelines.created(), 33, "retiring entries must not erase historical creations");
    }
    #[test]
    fn colour_fusion_program_bounds_and_boundaries() {
        assert!(source(&[4, 999]).is_none());
        assert!(source(&[]).is_none());
        assert!(source(&[1; MAX_STEPS + 1]).is_none());
        let p = params();
        let s = steps(&p);
        assert_eq!(program(&s).unwrap().0, vec![4, 8, 1, 5]);
        let p: Vec<_> = p.iter().cycle().take(68).cloned().collect();
        assert_eq!(program(&steps(&p)).unwrap().0.len(), MAX_STEPS);
        assert!(program(&s[..1]).is_none());
        let mut malformed = params();
        for id in ["rgb", "red", "green", "blue", "alpha"] {
            malformed[1].values.insert(id.into(), Value::Str("invalid".into()));
        }
        assert!(program(&steps(&malformed)).is_none(), "invalid Curves must keep its no-op semantics");
        let mut channels = params();
        channels[0].values.insert("redGamma".into(), Value::Scalar(1.5));
        assert!(program(&steps(&channels)).is_none(), "channel Levels remains its exact separate kernel");
        assert_eq!(program(&steps(&channels)[1..]).unwrap().0, vec![8, 1, 5], "rejected prefix leaves a compilable suffix");
        let long: Vec<_> = p.iter().cycle().take(68).cloned().collect();
        let long_steps = steps(&long);
        assert_eq!(program(&long_steps[32..]).unwrap().0.len(), 32);
        assert_eq!(program(&long_steps[64..]).unwrap().0.len(), 4);
    }
    #[test]
    fn colour_fusion_matches_cpu_and_separate_passes() {
        let gpu = crate::tests::gpu();
        assert!(gpu.is_some() || std::env::var("EFFECTCRAFT_REQUIRE_GPU").as_deref() != Ok("1"));
        let Some(g) = gpu else { return };
        let p = params();
        let steps = steps(&p);
        for levels in [Some(255.0), Some(32768.0), None] {
            let mut img = crate::tests::pattern(7, 17, 13);
            img.data[0] = [0.0; 4];
            img.data[1] = [2e-8, 5e-8, 8e-8, 1e-7];
            if levels.is_none() {
                img.data[2] = [-0.2, 1.4, 0.4, 0.8];
            }
            if let Some(l) = levels {
                effectcraft_render::color::quantize(&mut img, l);
            }
            let mut cpu = Buf { img: img.clone(), offset: [0.0; 2], scale: 1.0 };
            for step in &steps {
                cpu = effectcraft_effects::apply(step.spec, &step.ctx, cpu);
                if let Some(l) = levels {
                    effectcraft_render::color::quantize(&mut cpu.img, l);
                }
            }
            let input = g.ctx.upload_image(&img).unwrap();
            let mut e = Enc::new(&g.ctx);
            let (fused, count) = apply(&mut e, &steps, &input, levels).expect("four-step fusion");
            assert_eq!(count, 4);
            let fused = e.download(&fused).unwrap();
            let mut b = GBuf { img: input, offset: [0.0; 2], scale: 1.0 };
            for step in &steps {
                b = apply_chain(&mut e, std::slice::from_ref(step), b, levels).unwrap();
            }
            let separate = e.download(&b.img).unwrap();
            let tolerance = if levels == Some(255.0) { 1.0 / 255.0 + 1e-6 } else { 1e-3 };
            assert_pixels(&cpu.img, &fused, tolerance);
            assert_pixels(&separate, &fused, tolerance);
        }
    }
    #[cfg(feature = "profile")]
    #[test]
    fn colour_fusion_four_effects_one_dispatch() {
        let gpu = crate::tests::gpu();
        assert!(gpu.is_some() || std::env::var("EFFECTCRAFT_REQUIRE_GPU").as_deref() != Ok("1"));
        let Some(g) = gpu else { return };
        let p = params();
        let steps = steps(&p);
        let input = g.ctx.upload_image(&crate::tests::pattern(7, 17, 13)).unwrap();
        effectcraft_render::profile::start();
        let mut e = Enc::new(&g.ctx);
        let (out, count) = apply(&mut e, &steps, &input, Some(255.0)).unwrap();
        assert_eq!(count, 4);
        e.download(&out).unwrap();
        let (events, dropped) = effectcraft_render::profile::finish();
        assert_eq!(dropped, 0);
        if g.ctx.device.features().contains(wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS) {
            assert_eq!(events.iter().filter(|e| e.name == "colour_chain").count(), 1);
            assert!(!events.iter().any(|e| e.name == "pointwise" || e.name == "quantize"));
        }
    }
    #[test]
    fn colour_fusion_preserve_matches_separate_pass() {
        let gpu = crate::tests::gpu();
        assert!(gpu.is_some() || std::env::var("EFFECTCRAFT_REQUIRE_GPU").as_deref() != Ok("1"));
        let Some(g) = gpu else { return };
        let dst = g.ctx.upload_image(&crate::tests::pattern(1, 17, 13)).unwrap();
        let src = g.ctx.upload_image(&crate::tests::pattern(2, 17, 13)).unwrap();
        for levels in [Some(255.0), Some(32768.0), None] {
            for mode in [effectcraft_color::BlendMode::Normal, effectcraft_color::BlendMode::Screen] {
                let mut e = Enc::new(&g.ctx);
                let kept = e.scratch(17, 13);
                e.pixels("preserve", &Params::default(), &src, Some(&dst), &kept, None);
                let old = crate::ops::blend_full(&mut e, &dst, &kept, mode, 0.6, 11, levels);
                let new = crate::ops::blend_full_preserve(&mut e, &dst, &src, mode, 0.6, 11, levels, true);
                let old = e.download(&old).unwrap();
                let new = e.download(&new).unwrap();
                assert_pixels(&old, &new, if levels == Some(255.0) { 1.0 / 255.0 + 1e-6 } else { 1e-3 });
            }
        }
    }
    #[test]
    fn colour_fusion_long_stack_keeps_chunk_tail_and_dynamic_parameters() {
        let gpu = crate::tests::gpu();
        assert!(gpu.is_some() || std::env::var("EFFECTCRAFT_REQUIRE_GPU").as_deref() != Ok("1"));
        let Some(g) = gpu else { return };
        let spec = effectcraft_effects::find("ec.color.tint").unwrap();
        let params: Vec<_> = (0..36)
            .map(|i| {
                let mut p = FxParams { values: spec.params.iter().map(|p| (p.id.to_string(), effectcraft_effects::default_value(p, [17.0, 13.0]))).collect() };
                p.values.insert("amount".into(), Value::Scalar(2.0));
                p.values.insert("white".into(), Value::Color(if i % 2 == 0 { [1.0, 0.3, 0.1, 1.0] } else { [0.1, 0.8, 1.0, 1.0] }));
                p
            })
            .collect();
        let steps: Vec<_> = params
            .iter()
            .map(|p| FxStep { spec, ctx: EffectCtx { params: p, time: 0.0, layer_size: [17.0, 13.0], seed: 0, adjustment: false, env: EffectEnv::default() } })
            .collect();
        let mut cpu = Buf { img: crate::tests::pattern(7, 17, 13), offset: [0.0; 2], scale: 1.0 };
        let input = g.ctx.upload_image(&cpu.img).unwrap();
        for step in &steps {
            cpu = effectcraft_effects::apply(spec, &step.ctx, cpu);
        }
        let mut e = Enc::new(&g.ctx);
        let out = apply_chain(&mut e, &steps, GBuf { img: input, offset: [0.0; 2], scale: 1.0 }, None).unwrap();
        let out = e.download(&out.img).unwrap();
        assert_pixels(&cpu.img, &out, 1e-3);
        // A cached sequence must still use changed values, not baked first-use parameters.
        let mut changed = params.clone();
        for p in &mut changed {
            p.values.insert("amount".into(), Value::Scalar(0.0));
        }
        let steps: Vec<_> = changed
            .iter()
            .map(|p| FxStep { spec, ctx: EffectCtx { params: p, time: 0.0, layer_size: [17.0, 13.0], seed: 0, adjustment: false, env: EffectEnv::default() } })
            .collect();
        let original = crate::tests::pattern(7, 17, 13);
        let input = g.ctx.upload_image(&original).unwrap();
        let out = apply_chain(&mut e, &steps, GBuf { img: input, offset: [0.0; 2], scale: 1.0 }, None).unwrap();
        assert_pixels(&original, &e.download(&out.img).unwrap(), 1e-3);
    }
}
