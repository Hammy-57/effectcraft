//! The layer effect stack: the effect host (layer parameters, other times, the comp's
//! camera and lights), runs of GPU effects and the CPU effects, in effect space.

use crate::{Accelerator, EvalCtx, FxStep, FxTarget, Image, Renderer, masks, three_d};
use effectcraft_effects::{Buf, EffectCtx, EffectEnv, EffectHost, LayerPixels, Params};
use effectcraft_geom::{Mat3, vec2};
use effectcraft_project::{GroupKind, ItemKind, Layer, LayerSource};
use effectcraft_time::Tick;
use std::sync::Arc;

/// Layer parameters and audio for effects (see [`effectcraft_effects::EffectHost`]). What it
/// hands out is in effect space (see [`effect_bounds`]): the other layer's for its pixels, the
/// running layer's for its own frames and the comp scene.
struct FxHost<'r, 'a, 'c> {
    r: &'r Renderer<'a>,
    ctx: &'c EvalCtx<'a>,
    layer: &'c Layer,
    /// Where the stack's effect space starts in layer coordinates (zero on adjustment layers,
    /// whose effects run on the comp below).
    origin: [f64; 2],
    /// Index of the effect being rendered (bounds `self_at` to effects before it).
    index: std::sync::atomic::AtomicUsize,
}

impl FxHost<'_, '_, '_> {
    /// Another layer's pixels for a layer parameter, in its effect space; `layer_space`: `buf`
    /// is still in layer space (its source or its content).
    fn pixels(&self, other: &Layer, mut buf: Buf, layer_space: bool) -> LayerPixels {
        let (size, origin) = self.ctx.effect_bounds(other);
        if layer_space {
            buf.rebase(origin);
        }
        LayerPixels { buf, size }
    }
}

/// The comp's camera and first light relative to `layer`, in its layer pixels (see
/// [`EffectHost::comp_scene`]): the comp camera's view of the layer (as if 3D at its transform),
/// brought back into the layer's own pixel grid through the inverse of how the layer itself
/// composites.
pub(crate) fn comp_scene(ctx: &EvalCtx, layer: &Layer) -> Option<effectcraft_effects::CompScene> {
    let world = ctx.world_matrix(layer);
    let (cam, _, _) = ctx.camera();
    let p = (cam * world).0;
    let back = ctx.layer_to_comp(layer).0.inverse()?.0;
    let rows = [p[0], p[1], p[3]];
    let camera: [[f64; 4]; 3] = std::array::from_fn(|i| std::array::from_fn(|j| (0..3).map(|k| back[i][k] * rows[k][j]).sum()));
    let inv = world.inverse();
    let light = three_d::light::lights_at(ctx).first().and_then(|l| {
        let inv = inv.as_ref()?;
        let pos = inv.apply(l.pos);
        let dir = inv.apply_vec(l.dir);
        let len = (dir.x * dir.x + dir.y * dir.y + dir.z * dir.z).sqrt().max(1e-12);
        let kind = match l.kind {
            effectcraft_project::LightKind::Parallel => 0,
            effectcraft_project::LightKind::Ambient => 2,
            _ => 1,
        };
        Some(effectcraft_effects::CompLight { pos: [pos.x, pos.y, pos.z], dir: [dir.x / len, dir.y / len, dir.z / len], color: l.color, kind })
    });
    let camera_layer = ctx.comp.active_camera(ctx.time).is_some();
    Some(effectcraft_effects::CompScene { camera: Some(camera), camera_layer, light })
}

/// Nesting limit for effects that render layers (other layers, other times).
const MAX_FX_DEPTH: usize = 8;

impl EffectHost for FxHost<'_, '_, '_> {
    fn particles(&self) -> Option<&dyn effectcraft_effects::psim::ParticleSim> {
        self.r.active_accel().and_then(|a| a.particles())
    }
    fn layer(&self, id: u64, masks_and_effects: bool) -> Option<LayerPixels> {
        let other = self.ctx.layer(effectcraft_project::LayerId(id))?;
        if other.id == self.layer.id || self.r.depth > MAX_FX_DEPTH {
            return None;
        }
        let sub = self.r.nested();
        // The cached layer buffer is shared; effects get their own copy.
        let buf = if masks_and_effects { (*sub.content_buf(self.ctx, other)?).clone() } else { sub.source(self.ctx, other)? };
        Some(self.pixels(other, buf, true))
    }

    fn layer_masks(&self, id: u64) -> Option<LayerPixels> {
        let other = self.ctx.layer(effectcraft_project::LayerId(id))?;
        if other.id == self.layer.id || self.r.depth > MAX_FX_DEPTH {
            return None;
        }
        let sub = self.r.nested();
        let buf = (*sub.layer_input(self.ctx, other, 0)?).clone();
        Some(self.pixels(other, buf, false))
    }

    fn audio(&self, id: u64, start: f64, frames: usize, rate: u32) -> Option<Vec<f32>> {
        let other = self.ctx.layer(effectcraft_project::LayerId(id))?;
        let LayerSource::Footage { item } = &other.source else { return None };
        let ItemKind::Footage(f) = &self.r.project.item(*item)?.kind else { return None };
        if !f.has_audio {
            return None;
        }
        let st = other.layer_time(Tick::from_seconds_f64(start));
        self.r.footage.audio(*item, f, st, frames, rate)
    }

    fn self_at(&self, layer_time: f64, effects: usize) -> Option<Buf> {
        if self.r.depth > MAX_FX_DEPTH || self.layer.switches.adjustment {
            return None;
        }
        let n = effects.min(self.index.load(std::sync::atomic::Ordering::Relaxed));
        let t = self.layer.comp_time(Tick::from_seconds_f64(layer_time));
        let sub = self.r.nested();
        sub.layer_input(&self.ctx.at(t), self.layer, n).map(|b| (*b).clone())
    }

    fn aux(&self) -> Option<Arc<effectcraft_raster::AuxChannels>> {
        match &self.layer.source {
            LayerSource::Footage { item } => {
                let ItemKind::Footage(f) = &self.r.project.item(*item)?.kind else { return None };
                let a = self.r.footage.aux(*item, f, self.ctx.source_time(self.layer))?;
                // Aux pixels per layer pixel (the layer is the footage at its nominal size).
                if f.width > 0 && a.width > 0 {
                    let mut a = (*a).clone();
                    a.scale = a.width as f64 / f.width as f64;
                    return Some(Arc::new(a));
                }
                Some(a)
            }
            LayerSource::Comp { item } => {
                if self.r.depth > MAX_FX_DEPTH || self.r.project.comp_contains(*item, self.ctx.comp_id) {
                    return None;
                }
                self.r.nested().comp_aux(*item, self.ctx.nested_time(self.layer)).map(Arc::new)
            }
            _ => None,
        }
    }

    fn params_at(&self, layer_time: f64) -> Option<Params> {
        let i = self.index.load(std::sync::atomic::Ordering::Relaxed);
        let g = self.layer.effects()?.groups().nth(i)?;
        let ctx = self.ctx.at(self.layer.comp_time(Tick::from_seconds_f64(layer_time)));
        Some(effectcraft_effects::flatten_params(g, &mut |pr| ctx.value(self.layer, pr)))
    }

    fn comp_scene(&self) -> Option<effectcraft_effects::CompScene> {
        Some(comp_scene(self.ctx, self.layer)?.rebased(self.origin))
    }

    fn layer_at(&self, id: u64, comp_time: f64, masks_and_effects: bool) -> Option<LayerPixels> {
        let other = self.ctx.layer(effectcraft_project::LayerId(id))?;
        if other.id == self.layer.id || self.r.depth > MAX_FX_DEPTH {
            return None;
        }
        let ctx = self.ctx.at(Tick::from_seconds_f64(comp_time));
        let sub = self.r.nested();
        Some(if masks_and_effects {
            self.pixels(other, (*sub.content_buf(&ctx, other)?).clone(), true)
        } else {
            self.pixels(other, (*sub.layer_input(&ctx, other, 0)?).clone(), false)
        })
    }
}

/// An accelerator's kept effect result (see [`Accelerator::effects_kept`]): adopted by the
/// buffer that stores its pixels, released otherwise (when dropped).
pub(crate) struct Kept<'a> {
    accel: &'a dyn Accelerator,
    ticket: Option<u64>,
}

impl Kept<'_> {
    /// `buf` stores the kept pixels unchanged (offsets may differ).
    pub(crate) fn adopt(mut self, buf: &Arc<Buf>) {
        if let Some(t) = self.ticket.take() {
            self.accel.adopt(t, buf);
        }
    }
}

impl Drop for Kept<'_> {
    fn drop(&mut self) {
        if let Some(t) = self.ticket.take() {
            self.accel.release(t);
        }
    }
}

/// The CPU effect target (GPU chains go through [`Accelerator::effects`]: upload, readback).
struct CpuFx<'a> {
    accel: Option<&'a dyn Accelerator>,
    levels: Option<f32>,
    buf: Option<Buf>,
    /// The accelerator's copy of `buf`, while the last step was a GPU chain.
    kept: Option<Kept<'a>>,
}

impl FxTarget for CpuFx<'_> {
    fn gpu(&mut self, steps: &[FxStep]) -> bool {
        let (Some(a), Some(b)) = (self.accel, &self.buf) else { return false };
        match a.effects_kept(steps, b, self.levels) {
            Some((out, ticket)) => {
                self.buf = Some(out);
                self.kept = ticket.map(|t| Kept { accel: a, ticket: Some(t) });
                true
            }
            None => false,
        }
    }
    fn cpu(&mut self, f: &mut dyn FnMut(Buf) -> Buf) {
        if let Some(b) = self.buf.take() {
            self.kept = None;
            self.buf = Some(f(b));
        }
    }
    fn shift(&mut self, d: [f64; 2]) {
        if let Some(b) = &mut self.buf {
            b.offset = [b.offset[0] - d[0], b.offset[1] - d[1]];
        }
    }
}

impl<'a> Renderer<'a> {
    /// Evaluate an effect instance's parameters.
    pub(crate) fn effect_params(&self, ctx: &EvalCtx, layer: &Layer, g: &effectcraft_project::PropGroup) -> Params {
        // Nested groups (Paint strokes, Puppet meshes and pins) are flattened too.
        effectcraft_effects::flatten_params(g, &mut |pr| ctx.value(layer, pr))
    }

    pub(crate) fn apply_effects(&self, ctx: &EvalCtx, layer: &Layer, buf: Buf, adjustment: bool) -> Buf {
        self.apply_effects_timed(ctx, layer, buf, adjustment, usize::MAX, None)
    }

    /// Run the first `limit` effects of the layer's stack (by position; disabled ones count) on
    /// `buf` (layer space, or the comp below for an adjustment layer).
    pub(crate) fn apply_effects_timed(
        &self,
        ctx: &EvalCtx,
        layer: &Layer,
        buf: Buf,
        adjustment: bool,
        limit: usize,
        timing: Option<&mut Vec<(String, f64)>>,
    ) -> Buf {
        self.apply_effects_kept(ctx, layer, buf, adjustment, limit, timing).0
    }

    /// [`Renderer::apply_effects_timed`], with the accelerator's copy of the result when the
    /// stack ended with a GPU chain (see [`Kept`]).
    pub(crate) fn apply_effects_kept(
        &self,
        ctx: &EvalCtx,
        layer: &Layer,
        mut buf: Buf,
        adjustment: bool,
        limit: usize,
        timing: Option<&mut Vec<(String, f64)>>,
    ) -> (Buf, Option<Kept<'a>>) {
        let origin = fx_origin(ctx, layer, adjustment);
        buf.rebase(origin);
        let mut t = CpuFx { accel: self.active_accel(), levels: self.pipe.levels, buf: Some(buf), kept: None };
        self.run_effect_stack(ctx, layer, adjustment, limit, timing, origin, &mut t);
        let mut out = t.buf.take().unwrap_or_else(|| Buf { img: Image::new(1, 1), offset: [0.0; 2], scale: self.opts.scale });
        out.rebase([-origin[0], -origin[1]]);
        (out, t.kept.take())
    }

    /// [`Renderer::apply_effects_timed`] on a buffer already in effect space (measured from
    /// `origin`, in layer coordinates); the result stays in effect space.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn effect_space_stack(
        &self,
        ctx: &EvalCtx,
        layer: &Layer,
        buf: Buf,
        adjustment: bool,
        limit: usize,
        timing: Option<&mut Vec<(String, f64)>>,
        origin: [f64; 2],
    ) -> Buf {
        let mut t = CpuFx { accel: self.active_accel(), levels: self.pipe.levels, buf: Some(buf), kept: None };
        self.run_effect_stack(ctx, layer, adjustment, limit, timing, origin, &mut t);
        t.buf.unwrap_or_else(|| Buf { img: Image::new(1, 1), offset: [0.0; 2], scale: self.opts.scale })
    }

    /// Run the layer's video effect stack on `target` (in effect space, measured from `origin`
    /// in layer coordinates): runs of GPU-capable effects go to [`FxTarget::gpu`] (when an
    /// accelerator is active and supports them), the rest to [`FxTarget::cpu`] one by one
    /// (quantised to the bit depth after each).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn run_effect_stack(
        &self,
        ctx: &EvalCtx,
        layer: &Layer,
        adjustment: bool,
        limit: usize,
        mut timing: Option<&mut Vec<(String, f64)>>,
        origin: [f64; 2],
        target: &mut dyn FxTarget,
    ) {
        if !layer.switches.effects {
            return;
        }
        let Some(fx) = layer.effects() else { return };
        let lt = layer.layer_time(ctx.time);
        let layer_size = ctx.effect_bounds(layer).0;
        let mut mask_shapes = masks::shapes(ctx, layer);
        if origin != [0.0; 2] {
            for q in mask_shapes.iter_mut().flat_map(|m| m.points.iter_mut()) {
                *q = [q[0] - origin[0], q[1] - origin[1]];
            }
        }
        let host = FxHost { r: self, ctx, layer, origin, index: Default::default() };
        let env = EffectEnv {
            masks: &mask_shapes,
            host: Some(&host),
            comp_time: ctx.time.seconds(),
            frame_rate: ctx.comp.frame_rate.as_f64(),
            effect_index: 0,
            working_space: self.pipe.space,
            working_linear: self.pipe.linear,
            shutter: self.mb_on(ctx, layer).then_some((ctx.comp.shutter_angle, ctx.comp.shutter_phase, ctx.comp.motion_blur_samples)),
            // Comp space maps effect space, so it follows the origin.
            comp_space: fx_comp_space(ctx, layer).map(|m| {
                let m = m * Mat3::translate(vec2(origin[0], origin[1]));
                [m.0[0], m.0[1]]
            }),
        };
        // Video effects in stack order (index, group, spec); disabled and audio effects skipped.
        let stack: Vec<(usize, &effectcraft_project::PropGroup, &'static effectcraft_effects::EffectSpec)> = fx
            .groups()
            .enumerate()
            .take_while(|(i, _)| *i < limit)
            .filter(|(_, g)| g.enabled)
            .filter_map(|(i, g)| match &g.kind {
                GroupKind::Effect { effect } => effectcraft_effects::find(effect).map(|s| (i, g, s)),
                _ => None,
            })
            .filter(|(_, _, s)| !effectcraft_effects::audio_fx::is_audio_effect(s.id))
            .collect();
        let accel = self.active_accel();
        let mut k = 0;
        while k < stack.len() {
            let (i, g, spec) = stack[k];
            // A run of GPU effects goes to the accelerator in one go.
            if let Some(a) = accel
                && spec.gpu
                && a.supports_effect(spec.id)
            {
                let run: Vec<_> = stack[k..].iter().take_while(|(_, _, s)| s.gpu && a.supports_effect(s.id)).collect();
                let params: Vec<Params> = run.iter().map(|(_, g, _)| self.effect_params(ctx, layer, g)).collect();
                let steps: Vec<FxStep> = run
                    .iter()
                    .zip(&params)
                    .map(|((i, g, s), params)| FxStep {
                        spec: s,
                        ctx: EffectCtx { params, time: lt.seconds(), layer_size, seed: g.uid as u32, adjustment, env: EffectEnv { effect_index: *i, ..env } },
                    })
                    .collect();
                let t0 = web_time::Instant::now();
                let done = target.gpu(&steps);
                if done {
                    if let Some(v) = timing.as_deref_mut() {
                        let ms = t0.elapsed().as_secs_f64() * 1e3 / run.len() as f64;
                        v.extend(run.iter().map(|(_, _, s)| (format!("{} (gpu)", s.id), ms)));
                    }
                    k += run.len();
                    continue;
                }
            }
            k += 1;
            let params = self.effect_params(ctx, layer, g);
            host.index.store(i, std::sync::atomic::Ordering::Relaxed);
            let env = EffectEnv { effect_index: i, ..env };
            let ectx = EffectCtx { params: &params, time: lt.seconds(), layer_size, seed: g.uid as u32, adjustment, env };
            let t0 = web_time::Instant::now();
            target.cpu(&mut |buf| {
                let mut buf = effectcraft_effects::apply(spec, &ectx, buf);
                // 8/16 bpc effects write integer pixels.
                self.pipe.quantize(&mut buf.img);
                buf
            });
            if let Some(v) = timing.as_deref_mut() {
                v.push((spec.id.to_string(), t0.elapsed().as_secs_f64() * 1e3));
            }
        }
    }
}

/// Where a layer's effect space starts, in layer coordinates (see [`EvalCtx::effect_bounds`]).
/// The comp below an adjustment layer stays as it is, except that a shape layer's effects
/// always measure from its comp-sized canvas centred on the anchor (as After Effects does).
pub(crate) fn fx_origin(ctx: &EvalCtx, layer: &Layer, adjustment: bool) -> [f64; 2] {
    if adjustment && !matches!(layer.source, LayerSource::Shape) { [0.0; 2] } else { ctx.effect_bounds(layer).1 }
}

/// After Effects runs a Continuously Rasterize shape or text layer's effects in comp space:
/// measured with CC Light Sweep, its centre, angle and width stay put in the comp whatever the
/// layer's position, scale and rotation. The layer → comp affine of such a 2D layer (see
/// [`EffectEnv::comp_space`]).
pub(crate) fn fx_comp_space(ctx: &EvalCtx, layer: &Layer) -> Option<Mat3> {
    if !layer.switches.collapse || layer.is_3d() || !matches!(layer.source, LayerSource::Shape | LayerSource::Text) {
        return None;
    }
    Some(ctx.layer_to_comp(layer).0)
}
