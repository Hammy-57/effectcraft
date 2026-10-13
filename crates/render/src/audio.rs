//! Audio mixdown of a composition: every audible layer (footage with audio, precomps, and any
//! layer with a sound-generating effect such as Tone) summed with its Audio Levels (dB, per
//! channel), respecting in/out points, the Audio switch and solo. Layer audio effects
//! (Effect > Audio, see [`effectcraft_effects::audio_fx`]) run on each layer's audio before
//! its levels; Backwards plays the layer's [in, out) span reversed.
//!
//! Callers mix in short blocks (one video frame, or an audio device buffer). Effects and levels
//! are evaluated once per block. To make the result independent of block boundaries, layer
//! effects are run from a cleared state over a pre-roll of earlier audio and only the block's
//! tail is kept — so preview playback and export produce the same samples. A caller that mixes
//! consecutive blocks (playback) can hold an [`AudioStream`]: it keeps each layer's effect chain
//! running, so the next contiguous block with unchanged effect parameters only processes its own
//! samples (the result is what one long call from the first block would give) instead of the
//! pre-roll again (seconds of audio for a compressor or reverb, per 1024-frame block).
//!
//! Time-stretched layers are resampled nearest-sample; reversed (negative stretch) layers are
//! silent for now.

use std::sync::Mutex;

use effectcraft_effects::Params;
use effectcraft_effects::audio_fx::{self, Processor};
use effectcraft_project::{Comp, GroupKind, ItemId, ItemKind, Layer, LayerId, LayerSource, Node, Project};
use effectcraft_time::{TICKS_PER_SECOND, Tick};

use crate::{EvalCtx, ExprHost, FootageSource};

const MAX_DEPTH: usize = 16;

/// `frames` stereo sample frames of `comp`'s mix from comp time `start` at `rate` Hz,
/// interleaved (L R L R …).
pub fn mix_comp(project: &Project, footage: &dyn FootageSource, expr: Option<&dyn ExprHost>, comp: ItemId, start: Tick, frames: usize, rate: u32) -> Vec<f32> {
    let mut out = vec![0.0f32; frames * 2];
    mix_into(project, footage, expr, comp, start, rate, &mut out, 1.0, 1.0, 0, None);
    out
}

/// Layer effect chains kept running between consecutive [`AudioStream::mix`] calls, so playback
/// pays an effect's pre-roll once per seek instead of once per block.
///
/// Hold one per playback of one project snapshot (its state is the history of that project's
/// audio): a block that continues the previous one for a layer (it starts where that ended, or
/// within its last few frames) with the same effect parameters carries the chain on; anything
/// else (a seek or loop, a parameter change, Backwards) renders from a cleared state over the
/// pre-roll as [`mix_comp`] does and starts a new chain. The cache is bounded and shareable
/// between threads.
#[derive(Default)]
pub struct AudioStream {
    chains: Mutex<Chains>,
}

/// Chains kept at most.
const MAX_CHAINS: usize = 64;
/// Heap bytes of effect state (delay lines) kept at most.
const MAX_CHAIN_BYTES: usize = 64 << 20;
/// Output frames kept per chain, so a block that re-reads the end of the previous one (a mix
/// resampled to the device rate overlaps by a frame or two) still continues.
const TAIL_FRAMES: usize = 64;

#[derive(Default)]
struct Chains {
    list: Vec<Chain>,
    clock: u64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct ChainKey {
    /// The precomp nesting the layer is reached through (one chain per instance).
    path: u64,
    comp: ItemId,
    layer: LayerId,
    rate: u32,
}

/// One layer's running effect chain.
struct Chain {
    key: ChainKey,
    /// Effect ids and parameters the chain was built with.
    fx: Vec<(String, Params)>,
    procs: Vec<Processor>,
    /// Comp sample index of the next frame the chain would produce.
    end: i64,
    /// The last output frames (up to [`TAIL_FRAMES`]), ending at `end`.
    tail: Vec<f32>,
    used: u64,
}

impl Chain {
    fn bytes(&self) -> usize {
        self.procs.iter().map(Processor::state_bytes).sum::<usize>() + self.tail.capacity() * 4
    }

    fn keep_tail(&mut self, out: &[f32]) {
        self.tail.extend_from_slice(out);
        let extra = self.tail.len().saturating_sub(TAIL_FRAMES * 2);
        self.tail.drain(..extra);
    }
}

impl AudioStream {
    /// [`mix_comp`], continuing the layer effect chains of the previous calls where the block
    /// follows on.
    pub fn mix(
        &self,
        project: &Project,
        footage: &dyn FootageSource,
        expr: Option<&dyn ExprHost>,
        comp: ItemId,
        start: Tick,
        frames: usize,
        rate: u32,
    ) -> Vec<f32> {
        let mut out = vec![0.0f32; frames * 2];
        mix_into(project, footage, expr, comp, start, rate, &mut out, 1.0, 1.0, 0, Some((self, 0)));
        out
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Chains> {
        self.chains.lock().unwrap_or_else(|e| {
            // A panic mid-update leaves nothing half-written that matters: start over.
            let mut g = e.into_inner();
            g.list.clear();
            self.chains.clear_poison();
            g
        })
    }

    /// Take `key`'s chain out (it is put back after the block; the lock isn't held while
    /// rendering, which may mix precomps through this stream).
    fn take(&self, key: ChainKey) -> Option<Chain> {
        let mut g = self.lock();
        let i = g.list.iter().position(|c| c.key == key)?;
        Some(g.list.swap_remove(i))
    }

    fn put(&self, mut chain: Chain) {
        if chain.bytes() > MAX_CHAIN_BYTES {
            return;
        }
        let mut g = self.lock();
        g.clock = g.clock.saturating_add(1);
        chain.used = g.clock;
        g.list.retain(|c| c.key != chain.key);
        g.list.push(chain);
        while g.list.len() > MAX_CHAINS || g.list.iter().map(Chain::bytes).sum::<usize>() > MAX_CHAIN_BYTES {
            let Some(i) = g.list.iter().enumerate().min_by_key(|(_, c)| c.used).map(|(i, _)| i) else { break };
            g.list.swap_remove(i);
        }
    }

    /// Chains held (tests).
    #[cfg(test)]
    fn len(&self) -> usize {
        self.lock().list.len()
    }
}

/// Instance path of a precomp reached through layer `layer` of comp `comp` from `path`.
fn sub_path(path: u64, comp: ItemId, layer: LayerId) -> u64 {
    let mut h = path ^ 0x9e37_79b9_7f4a_7c15;
    for v in [comp.0, layer.0] {
        h = (h ^ v).wrapping_mul(0x0100_0000_01b3).rotate_left(29);
    }
    h
}

/// Whether the comp has anything audible (footage with audio, a sound generator, or a precomp
/// that has).
pub fn comp_has_audio(project: &Project, comp: ItemId) -> bool {
    fn walk(project: &Project, comp: ItemId, depth: usize) -> bool {
        let Some(c) = project.comp(comp) else { return false };
        depth < MAX_DEPTH
            && c.layers.iter().any(|l| {
                l.switches.audio
                    && (generates(l)
                        || match &l.source {
                            LayerSource::Footage { item } => {
                                matches!(project.item(*item).map(|i| &i.kind), Some(ItemKind::Footage(f)) if f.has_audio && !f.missing)
                            }
                            LayerSource::Comp { item } => walk(project, *item, depth + 1),
                            _ => false,
                        })
            })
    }
    walk(project, comp, 0)
}

/// Enabled audio effects of a layer (in stack order) with their effect ids.
fn audio_effect_groups(l: &Layer) -> impl Iterator<Item = (&str, &effectcraft_project::PropGroup)> {
    l.effects().filter(|_| l.switches.effects).into_iter().flat_map(|fx| fx.groups()).filter(|g| g.enabled).filter_map(|g| match &g.kind {
        GroupKind::Effect { effect } if audio_fx::is_audio_effect(effect) => Some((effect.as_str(), g)),
        _ => None,
    })
}

/// The layer makes sound on its own (Tone).
fn generates(l: &Layer) -> bool {
    audio_effect_groups(l).any(|(id, _)| audio_fx::generates_audio(id))
}

/// Does the layer contribute to the mix (Audio switch on and something to hear)?
pub fn audible(project: &Project, l: &Layer) -> bool {
    l.switches.audio
        && (generates(l)
            || match &l.source {
                LayerSource::Footage { item } => matches!(project.item(*item).map(|i| &i.kind), Some(ItemKind::Footage(f)) if f.has_audio),
                LayerSource::Comp { .. } => true,
                _ => false,
            })
}

/// Linear gain of an Audio Levels value in dB; -96 dB and below is silence.
pub fn db_to_gain(db: f64) -> f32 {
    if db <= -96.0 { 0.0 } else { 10f64.powf(db / 20.0) as f32 }
}

fn units_ceil(t: Tick, rate: u32) -> i64 {
    let n = t.0 as i128 * rate as i128;
    let d = TICKS_PER_SECOND as i128;
    (n.div_euclid(d) + i128::from(n.rem_euclid(d) != 0)) as i64
}

fn sample_tick(i: i64, rate: u32) -> Tick {
    // Keep the requested sample when the rate does not divide the tick clock.
    let t = Tick::from_units(i, i64::from(rate));
    if t.to_units_floor(i64::from(rate)) < i { Tick(t.0.saturating_add(1)) } else { t }
}

struct MixEnv<'a> {
    project: &'a Project,
    footage: &'a dyn FootageSource,
    expr: Option<&'a dyn ExprHost>,
    comp_id: ItemId,
    comp: &'a Comp,
    rate: u32,
    depth: usize,
    /// The stream continuing effect chains, and the precomp instance path of this comp.
    stream: Option<(&'a AudioStream, u64)>,
}

impl MixEnv<'_> {
    fn ctx(&self, t: Tick) -> EvalCtx<'_> {
        let mut ctx = EvalCtx::new(self.project, self.comp_id, self.comp, t);
        ctx.expr = self.expr;
        ctx
    }

    /// The layer's own (pre-effect) audio for comp sample indices `i0 .. i0 + n`, silent
    /// outside its [in, out) span. Comp sample index `k` maps to `in + out − 1 − k` when
    /// `reversed`.
    fn layer_raw(&self, l: &Layer, i0: i64, n: usize, reversed: bool) -> Vec<f32> {
        let rate = self.rate;
        let mut out = vec![0.0f32; n * 2];
        let (in_i, out_i) = (units_ceil(l.in_point, rate), units_ceil(l.out_point, rate));
        if reversed {
            // Mirror the clipped range, fetch it forwards, reverse.
            let a = i0.max(in_i);
            let b = (i0 + n as i64).min(out_i);
            if a >= b {
                return out;
            }
            let (ma, mb) = (in_i + out_i - b, in_i + out_i - a);
            let fwd = self.layer_raw(l, ma, (mb - ma) as usize, false);
            for k in a..b {
                let j = (in_i + out_i - 1 - k - ma) as usize;
                let o = (k - i0) as usize;
                out[o * 2] = fwd[j * 2];
                out[o * 2 + 1] = fwd[j * 2 + 1];
            }
            return out;
        }
        let a = (in_i - i0).clamp(0, n as i64) as usize;
        let b = (out_i - i0).clamp(0, n as i64) as usize;
        if a >= b {
            return out;
        }
        let m = b - a;
        let speed = 100.0 / l.stretch;
        let src_n = ((m as f64 * speed).ceil() as usize).max(1);
        let lt = l.layer_time(sample_tick(i0 + a as i64, rate));
        let buf = match &l.source {
            LayerSource::Footage { item } => match self.project.item(*item).map(|i| &i.kind) {
                Some(ItemKind::Footage(f)) if f.has_audio => self.footage.audio(*item, f, lt, src_n, rate).unwrap_or_else(|| vec![0.0; src_n * 2]),
                _ => return out,
            },
            LayerSource::Comp { item } => {
                if self.project.comp_contains(*item, self.comp_id) {
                    return out;
                }
                let mut sub = vec![0.0f32; src_n * 2];
                let stream = self.stream.map(|(s, path)| (s, sub_path(path, self.comp_id, l.id)));
                mix_into(self.project, self.footage, self.expr, *item, lt, rate, &mut sub, 1.0, 1.0, self.depth + 1, stream);
                sub
            }
            _ => return out,
        };
        let same = (speed - 1.0).abs() < 1e-9;
        for i in 0..m {
            let x = i as f64 * speed;
            let j = if same { i } else { (x as usize).min(src_n - 1) };
            if j * 2 + 1 >= buf.len() {
                break;
            }
            let k = if same || (j + 1) * 2 + 1 >= buf.len() { j } else { j + 1 };
            let f = if same { 0.0 } else { ((x - j as f64) as f32).clamp(0.0, 1.0) };
            out[(a + i) * 2] = buf[j * 2] + (buf[k * 2] - buf[j * 2]) * f;
            out[(a + i) * 2 + 1] = buf[j * 2 + 1] + (buf[k * 2 + 1] - buf[j * 2 + 1]) * f;
        }
        out
    }

    /// Effect parameters at comp time `t`.
    fn params(&self, l: &Layer, g: &effectcraft_project::PropGroup, t: Tick) -> Params {
        let ctx = self.ctx(t);
        let mut p = Params::default();
        for c in &g.children {
            if let Node::Prop(pr) = c {
                p.values.insert(pr.match_id.clone(), ctx.value(l, pr));
            }
        }
        p
    }

    /// The layer's processed audio (effects applied, levels not) for comp sample indices
    /// `i0 .. i0 + n`.
    fn layer_audio(&self, l: &Layer, i0: i64, n: usize) -> Vec<f32> {
        let t = sample_tick(i0, self.rate);
        let fx: Vec<(&str, Params)> = audio_effect_groups(l).map(|(id, g)| (id, self.params(l, g, t))).collect();
        if fx.is_empty() {
            return self.layer_raw(l, i0, n, false);
        }
        let reversed = fx.iter().any(|(id, _)| audio_fx::reverses(id));
        // Backwards plays the span mirrored: consecutive blocks don't continue one signal.
        let Some((stream, path)) = self.stream.filter(|_| !reversed) else { return self.fresh_audio(l, &fx, i0, n, reversed).0 };
        let key = ChainKey { path, comp: self.comp_id, layer: l.id, rate: self.rate };
        if let Some(mut chain) = stream.take(key)
            && let Some(out) = self.continue_chain(l, &mut chain, &fx, i0, n)
        {
            stream.put(chain);
            return out;
        }
        let (out, procs) = self.fresh_audio(l, &fx, i0, n, false);
        let fx = fx.into_iter().map(|(id, p)| (id.to_string(), p)).collect();
        let mut chain = Chain { key, fx, procs, end: i0.saturating_add(n as i64), tail: vec![], used: 0 };
        chain.keep_tail(&out);
        stream.put(chain);
        out
    }

    /// The layer's processed audio for `i0 .. i0 + n` from a cleared state over the effects'
    /// pre-roll, and the processors left running at the block's end.
    fn fresh_audio(&self, l: &Layer, fx: &[(&str, Params)], i0: i64, n: usize, reversed: bool) -> (Vec<f32>, Vec<Processor>) {
        let pre_s: f64 = fx.iter().map(|(id, p)| audio_fx::preroll(id, p)).sum();
        let pre = (pre_s * self.rate as f64).ceil() as usize;
        let mut buf = self.layer_raw(l, i0 - pre as i64, n + pre, reversed);
        let start = l.layer_time(sample_tick(i0 - pre as i64, self.rate)).seconds();
        let mut procs: Vec<Processor> = fx.iter().filter_map(|(id, p)| Processor::new(id, p, self.rate, start)).collect();
        for p in &mut procs {
            p.process_block(&mut buf);
        }
        buf.drain(..pre * 2);
        (buf, procs)
    }

    /// The block from `chain` when it follows on (starts at the chain's end or within its kept
    /// tail, same effects and parameters); `None` to render it fresh.
    fn continue_chain(&self, l: &Layer, chain: &mut Chain, fx: &[(&str, Params)], i0: i64, n: usize) -> Option<Vec<f32>> {
        let same = chain.fx.len() == fx.len() && chain.fx.iter().zip(fx).all(|((a, pa), (b, pb))| a == b && pa.values == pb.values);
        let back = usize::try_from(chain.end.checked_sub(i0)?).ok()?;
        let tail_frames = chain.tail.len() / 2;
        if !same || back > tail_frames {
            return None;
        }
        // Frames of the block the chain already produced, then the new ones.
        let from = (tail_frames - back) * 2;
        let mut out = Vec::with_capacity(n * 2);
        out.extend_from_slice(chain.tail.get(from..from + back.min(n) * 2)?);
        let m = n - back.min(n);
        if m > 0 {
            let mut buf = self.layer_raw(l, chain.end, m, false);
            for p in &mut chain.procs {
                p.process_block(&mut buf);
            }
            chain.end = chain.end.saturating_add(m as i64);
            chain.keep_tail(&buf);
            out.extend_from_slice(&buf);
        }
        Some(out)
    }
}

#[allow(clippy::too_many_arguments)]
fn mix_into(
    project: &Project,
    footage: &dyn FootageSource,
    expr: Option<&dyn ExprHost>,
    comp_id: ItemId,
    start: Tick,
    rate: u32,
    out: &mut [f32],
    gl: f32,
    gr: f32,
    depth: usize,
    stream: Option<(&AudioStream, u64)>,
) {
    let Some(comp) = project.comp(comp_id) else { return };
    if depth >= MAX_DEPTH || rate == 0 {
        return;
    }
    let env = MixEnv { project, footage, expr, comp_id, comp, rate, depth, stream };
    let frames = out.len() / 2;
    let any_solo = comp.layers.iter().any(|l| l.switches.solo && audible(project, l));
    let s0 = start.to_units_floor(rate as i64);
    for l in &comp.layers {
        if !audible(project, l) || (any_solo && !l.switches.solo) || l.stretch <= 0.0 {
            continue;
        }
        // Sample range of the block that falls inside the layer's [in, out).
        let a = (units_ceil(l.in_point, rate) - s0).clamp(0, frames as i64) as usize;
        let b = (units_ceil(l.out_point, rate) - s0).clamp(0, frames as i64) as usize;
        if a >= b {
            continue;
        }
        let t_a = sample_tick(s0 + a as i64, rate);
        let (ll, lr) = levels(&env, l, t_a);
        let (ll, lr) = (ll * gl, lr * gr);
        if ll == 0.0 && lr == 0.0 {
            continue;
        }
        let buf = env.layer_audio(l, s0 + a as i64, b - a);
        for (i, f) in buf.as_chunks::<2>().0.iter().enumerate() {
            out[(a + i) * 2] += f[0] * ll;
            out[(a + i) * 2 + 1] += f[1] * lr;
        }
    }
}

fn levels(env: &MixEnv, l: &Layer, t: Tick) -> (f32, f32) {
    let Some(g) = l.props.sub("audio") else { return (1.0, 1.0) };
    let [a, b] = env.ctx(t).v2(l, g, "levels", [0.0, 0.0]);
    (db_to_gain(a), db_to_gain(b))
}

/// A min/max peak summary of interleaved stereo audio for waveform drawing: one entry per
/// `bin` sample frames, `[min L, max L, min R, max R]`.
pub fn peak_summary(samples: &[f32], bin: usize) -> Vec<[f32; 4]> {
    let bin = bin.max(1);
    samples
        .chunks(bin * 2)
        .map(|c| {
            let mut s = [f32::MAX, f32::MIN, f32::MAX, f32::MIN];
            for f in c.as_chunks::<2>().0 {
                s[0] = s[0].min(f[0]);
                s[1] = s[1].max(f[0]);
                s[2] = s[2].min(f[1]);
                s[3] = s[3].max(f[1]);
            }
            if s[0] > s[1] { [0.0; 4] } else { s }
        })
        .collect()
}

/// Peak summary of a footage item's audio from source time 0 over `duration`, `bins_per_sec`
/// entries per second (rendered at a low internal rate, fetched in one-second chunks).
pub fn footage_peaks(footage: &dyn FootageSource, item: ItemId, f: &effectcraft_project::Footage, duration: Tick, bins_per_sec: u32) -> Vec<[f32; 4]> {
    let bins_per_sec = bins_per_sec.max(1);
    // Enough samples per bin to catch peaks of audible content, without decoding at full rate.
    let bin = (64usize).max(4000 / bins_per_sec as usize);
    let rate = bin as u32 * bins_per_sec;
    let chunk = bin * bins_per_sec as usize;
    let total = units_ceil(duration, rate).max(0) as usize;
    let mut out = Vec::with_capacity(total / bin + 1);
    let mut i = 0usize;
    while i < total {
        let n = chunk.min(total - i);
        let buf = footage.audio(item, f, sample_tick(i as i64, rate), n, rate).unwrap_or_else(|| vec![0.0; n * 2]);
        out.extend(peak_summary(&buf, bin));
        i += n;
    }
    out
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use effectcraft_project::{Footage, FootageKind, build};
    use effectcraft_time::FrameRate;

    use super::*;
    use crate::Image;

    /// A 1 kHz-ish constant "tone": every sample is 0.25 (left) / -0.25 (right).
    struct Dc;
    impl FootageSource for Dc {
        fn frame(&self, _: ItemId, _: &Footage, _: Tick) -> Option<Arc<Image>> {
            None
        }
        fn audio(&self, _: ItemId, _: &Footage, start: Tick, frames: usize, _: u32) -> Option<Vec<f32>> {
            Some((0..frames).flat_map(|_| if start >= Tick::ZERO { [0.25, -0.25] } else { [0.0, 0.0] }).collect())
        }
    }

    #[test]
    fn mixes_layers_with_levels_and_in_out() {
        let mut p = Project::default();
        let f = Footage {
            path: "x.wav".into(),
            kind: FootageKind::Audio,
            width: 0,
            height: 0,
            pixel_aspect: 1.0,
            frame_rate: FrameRate::new(25, 1),
            native_rate: None,
            duration: Tick::from_seconds_f64(10.0),
            has_video: false,
            has_audio: true,
            alpha: Default::default(),
            premul_color: [0.0; 3],
            loop_count: 1,
            codec: String::new(),
            missing: false,
            sequence: vec![],
            color_profile: None,
            ..Default::default()
        };
        let fid = p.add_item("x.wav", effectcraft_color::Label::SeaFoam, None, ItemKind::Footage(f));
        let comp = Comp::new(64, 64, FrameRate::new(25, 1), Tick::from_seconds_f64(2.0));
        let mut l1 = build::layer(&mut p, &comp, "a", LayerSource::Footage { item: fid }, (0, 0), None);
        l1.in_point = Tick::from_seconds_f64(1.0);
        let mut l2 = build::layer(&mut p, &comp, "b", LayerSource::Footage { item: fid }, (0, 0), None);
        if let Some(g) = l2.props.sub_mut("audio")
            && let Some(pr) = g.get_mut("levels")
        {
            pr.value = effectcraft_project::Value::Vec2([-6.0206, -96.0]);
        }
        let mut comp = comp;
        comp.layers = vec![l1, l2].into();
        let cid = p.add_item("C", effectcraft_color::Label::Sandstone, None, ItemKind::Comp(comp.into()));
        assert!(comp_has_audio(&p, cid));
        let rate = 1000;
        let m = mix_comp(&p, &Dc, None, cid, Tick::from_seconds_f64(0.5), 1000, rate);
        // first half: only layer b (half gain left, muted right)
        assert!((m[0] - 0.125).abs() < 1e-3 && m[1].abs() < 1e-6, "{} {}", m[0], m[1]);
        // second half (≥ 1 s): a + b
        let i = 700 * 2;
        assert!((m[i] - 0.375).abs() < 1e-3 && (m[i + 1] + 0.25).abs() < 1e-3, "{} {}", m[i], m[i + 1]);
    }

    /// Noise footage addressed by absolute sample index (so any split of a range reads the same
    /// samples), counting the frames read.
    #[derive(Default)]
    struct Noise(std::sync::atomic::AtomicUsize);
    impl FootageSource for Noise {
        fn frame(&self, _: ItemId, _: &Footage, _: Tick) -> Option<Arc<Image>> {
            None
        }
        fn audio(&self, _: ItemId, _: &Footage, start: Tick, frames: usize, rate: u32) -> Option<Vec<f32>> {
            self.0.fetch_add(frames, std::sync::atomic::Ordering::Relaxed);
            let s0 = start.to_units_floor(rate as i64);
            Some(
                (0..frames as i64)
                    .flat_map(|i| {
                        let k = s0 + i;
                        if k < 0 {
                            return [0.0, 0.0];
                        }
                        let x = ((k as f64 * 12.9898).sin() * 43758.5453).fract() as f32;
                        // Loud and quiet passages, so dynamics effects move.
                        let env = if (k / 900) % 3 == 0 { 0.9 } else { 0.05 };
                        [x * env, -x * env * 0.5]
                    })
                    .collect(),
            )
        }
    }

    impl Noise {
        fn read(&self) -> usize {
            self.0.swap(0, std::sync::atomic::Ordering::Relaxed)
        }
    }

    const SR: u32 = 8000;
    const BLOCK: usize = 1024;

    /// A comp with one footage-audio layer (or a solid, for Tone) carrying effect `id`.
    fn fx_comp(id: &str, vals: &[(&str, effectcraft_project::Value)]) -> (Project, ItemId) {
        let mut p = Project::default();
        let comp = Comp::new(64, 64, FrameRate::new(25, 1), Tick::from_seconds_f64(20.0));
        let src = if id == "ec.audio.tone" {
            let sid = p.add_item(
                "S",
                effectcraft_color::Label::Red,
                None,
                ItemKind::Solid(effectcraft_project::Solid { color: [1.0; 3], width: 64, height: 64, pixel_aspect: 1.0 }),
            );
            LayerSource::Solid { item: sid }
        } else {
            let f = Footage {
                kind: FootageKind::Audio,
                frame_rate: FrameRate::new(25, 1),
                duration: Tick::from_seconds_f64(20.0),
                has_audio: true,
                ..Default::default()
            };
            LayerSource::Footage { item: p.add_item("n.wav", effectcraft_color::Label::SeaFoam, None, ItemKind::Footage(f)) }
        };
        let mut l = build::layer(&mut p, &comp, "a", src, (64, 64), None);
        let spec = effectcraft_effects::find(id).unwrap();
        let mut next = p.next_id;
        let mut g = effectcraft_effects::instantiate(spec, &mut build::Ids(&mut next), spec.name, [64.0, 64.0]);
        p.next_id = next;
        for (k, v) in vals {
            g.prop_mut(k).unwrap().value = v.clone();
        }
        l.props.sub_mut("effects").unwrap().children.push(g.into());
        let mut comp = comp;
        comp.layers = vec![l].into();
        let cid = p.add_item("C", effectcraft_color::Label::Sandstone, None, ItemKind::Comp(comp.into()));
        (p, cid)
    }

    fn at(i: i64) -> Tick {
        sample_tick(i, SR)
    }

    fn bits(b: &[f32]) -> Vec<u32> {
        b.iter().map(|v| v.to_bits()).collect()
    }

    fn streamable() -> Vec<(&'static str, Vec<(&'static str, effectcraft_project::Value)>)> {
        use effectcraft_project::Value as V;
        vec![
            ("ec.audio.basstreble", vec![("bass", V::Scalar(50.0))]),
            ("ec.audio.compressor", vec![]),
            ("ec.audio.compressor", vec![("autoRelease", V::Bool(false)), ("threshold", V::Scalar(-30.0))]),
            ("ec.audio.delay", vec![("delayTime", V::Scalar(90.0))]),
            ("ec.audio.distortion", vec![("downsample", V::Scalar(3.0))]),
            ("ec.audio.flangechorus", vec![]),
            ("ec.audio.gate", vec![("threshold", V::Scalar(-12.0))]),
            ("ec.audio.highlowpass", vec![]),
            ("ec.audio.modulator", vec![("amplitudeModulation", V::Scalar(30.0))]),
            ("ec.audio.parametriceq", vec![("band1BoostCut", V::Scalar(9.0))]),
            ("ec.audio.reverb", vec![]),
            ("ec.audio.stereomixer", vec![("leftPan", V::Scalar(40.0))]),
            ("ec.audio.tone", vec![]),
        ]
    }

    /// Playback-sized blocks through an [`AudioStream`] equal one long mix of the whole range
    /// bit for bit, stay within the block-boundary tolerance of per-block fresh mixes, and read
    /// only each block's own samples after the first (no pre-roll).
    #[test]
    fn stream_blocks_match_one_long_mix() {
        let i0 = 1601i64;
        let blocks = 8;
        for (id, vals) in streamable() {
            let (p, cid) = fx_comp(id, &vals);
            let src = Noise::default();
            let long = mix_comp(&p, &src, None, cid, at(i0), BLOCK * blocks, SR);
            let stream = AudioStream::default();
            let (mut streamed, mut fresh) = (vec![], vec![]);
            for k in 0..blocks {
                let s = i0 + (k * BLOCK) as i64;
                src.read();
                streamed.extend(stream.mix(&p, &src, None, cid, at(s), BLOCK, SR));
                let read = src.read();
                if k > 0 && id != "ec.audio.tone" {
                    assert_eq!(read, BLOCK, "{id}: block {k} read {read} frames");
                }
                fresh.extend(mix_comp(&p, &src, None, cid, at(s), BLOCK, SR));
            }
            assert_eq!(bits(&streamed), bits(&long), "{id}");
            let worst = streamed.iter().zip(&fresh).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
            assert!(worst < 2e-3, "{id}: {worst}");
            assert_eq!(stream.len(), 1);
        }
    }

    /// A seek (back or forward), a parameter change or a re-read overlapping the previous block
    /// falls back or continues correctly.
    #[test]
    fn stream_seek_param_change_and_overlap() {
        let (p, cid) = fx_comp("ec.audio.compressor", &[]);
        let src = Noise::default();
        let stream = AudioStream::default();
        for k in 0..4 {
            stream.mix(&p, &src, None, cid, at(8000 + k * BLOCK as i64), BLOCK, SR);
        }
        // Jump back: the block renders fresh (pre-roll), then continues from there.
        let back = stream.mix(&p, &src, None, cid, at(2000), BLOCK, SR);
        assert_eq!(bits(&back), bits(&mix_comp(&p, &src, None, cid, at(2000), BLOCK, SR)));
        let next = stream.mix(&p, &src, None, cid, at(2000 + BLOCK as i64), BLOCK, SR);
        let long = mix_comp(&p, &src, None, cid, at(2000), 2 * BLOCK, SR);
        assert_eq!(bits(&next), bits(&long[BLOCK * 2..]));
        // A block that re-reads the previous one's last frames (resampled playback) continues.
        let ov = stream.mix(&p, &src, None, cid, at(2000 + 2 * BLOCK as i64 - 3), BLOCK, SR);
        let long = mix_comp(&p, &src, None, cid, at(2000), 3 * BLOCK - 3, SR);
        assert_eq!(bits(&ov), bits(&long[(2 * BLOCK - 3) * 2..]));
        // Changed parameters: the contiguous block renders fresh with the new values.
        // (The same comp and layer ids, another threshold.)
        let (p2, cid2) = fx_comp("ec.audio.compressor", &[("threshold", effectcraft_project::Value::Scalar(-40.0))]);
        assert_eq!(cid2, cid);
        let s = 2000 + 3 * BLOCK as i64 - 3;
        let changed = stream.mix(&p2, &src, None, cid, at(s), BLOCK, SR);
        assert_eq!(bits(&changed), bits(&mix_comp(&p2, &src, None, cid, at(s), BLOCK, SR)));
        assert_ne!(bits(&changed), bits(&mix_comp(&p, &src, None, cid, at(s), BLOCK, SR)));
    }

    /// Backwards (a mirrored time mapping) never continues a chain: every block is fresh.
    #[test]
    fn stream_backwards_renders_fresh() {
        let (p, cid) = fx_comp("ec.audio.backwards", &[]);
        let src = Noise::default();
        let stream = AudioStream::default();
        for k in 0..3 {
            let s = 4000 + k * BLOCK as i64;
            let a = stream.mix(&p, &src, None, cid, at(s), BLOCK, SR);
            assert_eq!(bits(&a), bits(&mix_comp(&p, &src, None, cid, at(s), BLOCK, SR)));
        }
        assert_eq!(stream.len(), 0);
    }
    /// A 440 Hz tone, so a change in the mix shows in the samples.
    struct Tone;
    impl FootageSource for Tone {
        fn frame(&self, _: ItemId, _: &Footage, _: Tick) -> Option<Arc<Image>> {
            None
        }
        fn audio(&self, _: ItemId, _: &Footage, start: Tick, frames: usize, rate: u32) -> Option<Vec<f32>> {
            let t0 = start.seconds();
            Some(
                (0..frames)
                    .flat_map(|k| {
                        let v = (std::f64::consts::TAU * 440.0 * (t0 + k as f64 / rate as f64)).sin() as f32 * 0.5;
                        [v, v]
                    })
                    .collect(),
            )
        }
    }

    #[test]
    fn a_plain_precomp_mixes_the_same_audio_as_its_layers() {
        let mut p = Project::default();
        let f = Footage {
            path: "tone.wav".into(),
            kind: FootageKind::Audio,
            has_audio: true,
            frame_rate: FrameRate::new(25, 1),
            duration: Tick::from_seconds_f64(10.0),
            ..Default::default()
        };
        let fid = p.add_item("tone.wav", effectcraft_color::Label::SeaFoam, None, ItemKind::Footage(f));
        let base = Comp::new(64, 64, FrameRate::new(25, 1), Tick::from_seconds_f64(2.0));
        let tone = build::layer(&mut p, &base, "tone", LayerSource::Footage { item: fid }, (0, 0), None);
        let mut inner = base.clone();
        inner.layers = vec![tone].into();
        let inner_id = p.add_item("Inner", effectcraft_color::Label::Sandstone, None, ItemKind::Comp(inner.into()));
        let pre = build::layer(&mut p, &base, "pre", LayerSource::Comp { item: inner_id }, (64, 64), None);
        let mut outer = base.clone();
        outer.layers = vec![pre].into();
        let outer_id = p.add_item("Outer", effectcraft_color::Label::Sandstone, None, ItemKind::Comp(outer.into()));
        let rate = 8000;
        let t = Tick::from_seconds_f64(0.5);
        let direct = mix_comp(&p, &Tone, None, inner_id, t, 4000, rate);
        let nested = mix_comp(&p, &Tone, None, outer_id, t, 4000, rate);
        assert_eq!(direct.len(), nested.len());
        let worst = direct.iter().zip(&nested).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
        assert!(worst < 1e-6, "a plain precomp changes its audio by {worst}");
    }

    /// The error of a tone mixed at `stretch` percent against the exact tone, at `rate` Hz.
    fn stretch_error(stretch: f64, rate: u32) -> (f64, f64) {
        let mut p = Project::default();
        let f = Footage {
            path: "tone.wav".into(),
            kind: FootageKind::Audio,
            has_audio: true,
            frame_rate: FrameRate::new(25, 1),
            duration: Tick::from_seconds_f64(10.0),
            ..Default::default()
        };
        let fid = p.add_item("tone.wav", effectcraft_color::Label::SeaFoam, None, ItemKind::Footage(f));
        let base = Comp::new(64, 64, FrameRate::new(25, 1), Tick::from_seconds_f64(2.0));
        let mut tone = build::layer(&mut p, &base, "tone", LayerSource::Footage { item: fid }, (0, 0), None);
        tone.stretch = stretch;
        let mut comp = base.clone();
        comp.layers = vec![tone].into();
        let cid = p.add_item("C", effectcraft_color::Label::Sandstone, None, ItemKind::Comp(comp.into()));
        let n = 2000usize;
        let t = Tick::from_seconds_f64(0.5);
        let m = mix_comp(&p, &Tone, None, cid, t, n, rate);
        let speed = 100.0 / stretch;
        let (mut worst, mut sq) = (0.0f64, 0.0f64);
        for i in 0..n {
            // The source plays `speed` times as fast as the comp: source time = speed * comp time.
            let ideal = (std::f64::consts::TAU * 440.0 * speed * (t.seconds() + i as f64 / rate as f64)).sin() * 0.5;
            let e = (m[i * 2] as f64 - ideal).abs();
            worst = worst.max(e);
            sq += e * e;
        }
        (worst, (sq / n as f64).sqrt())
    }

    #[test]
    fn stretched_layers_resample_a_tone_with_small_error() {
        // Blending the two samples around each time: about 0.0002 at 48 kHz and 0.007 at 8 kHz. Copying
        // the nearest sample gave 0.025 and 0.15.
        for stretch in [70.0, 33.0] {
            let (worst, _) = stretch_error(stretch, 48_000);
            assert!(worst < 1e-3, "stretch {stretch}%: error {worst} at 48 kHz");
            let (worst, _) = stretch_error(stretch, 8_000);
            assert!(worst < 1e-2, "stretch {stretch}%: error {worst} at 8 kHz");
        }
    }
}
