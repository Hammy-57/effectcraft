//! Minification chains kept with retained images.
//!
//! A layer drawn below half size samples a box-filtered level of its image (`ops::warp_q`): the
//! image halved until the remaining scale is at least ½. A cached layer is drawn from the same
//! retained image frame after frame, so its levels are the same every frame too: they are built
//! once and kept with the image instead of being rebuilt for every frame (the same `half` kernel
//! on the same input, so the same pixels).
//!
//! Only images that stay unchanged for as long as they live are sealed ([`Mips::seal`]: uploaded
//! layer buffers, retained processed and native results); a working image never is. Levels an
//! encoder builds stay private to it until its work has been submitted ([`publish`], from
//! `Enc::publish_native`), so no other encoder reads a level before the commands writing it ran.
//! A published level is an owner slot like the image it came from: it counts against the same
//! shared GPU budget, and the layer cache evicts it under pressure (the next use rebuilds it). A
//! chain goes with its image (pruned once the image's allocation is gone) and with the upload
//! caches (`GpuContext::clear_uploads`).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};

use effectcraft_render::allocation::GpuAllocation;
use effectcraft_render::cache::{GpuOwner, GpuSlot, GpuStage};

use crate::context::{Enc, GpuImage};

/// Sealed images at most (as many as an owner retains).
const MAX_CHAINS: usize = effectcraft_render::retained::MAX_RECORDS;

/// Levels kept per image at most (a 2^32-pixel side halves to one pixel in 32 steps).
const MAX_LEVELS: usize = 32;

/// A published level: its slot in the owner that retains it.
struct Level {
    owner: GpuOwner,
    slot: Weak<GpuSlot>,
}

impl Level {
    fn release(self) {
        self.owner.release(&self.slot);
    }
}

/// A sealed image's levels (`levels[i]`: the image halved `i + 1` times).
struct Chain {
    source: Weak<GpuAllocation>,
    levels: Vec<Option<Level>>,
}

impl Chain {
    fn release(self) {
        self.levels.into_iter().flatten().for_each(Level::release);
    }
}

/// The context's chains, by the address of the sealed image's allocation (every clone of an image
/// shares it; the chain's weak reference keeps the address from being reused while it exists).
#[derive(Default)]
pub(crate) struct Mips {
    chains: Mutex<HashMap<usize, Chain>>,
}

/// Levels an encoder built for sealed images, by image, until [`publish`].
#[derive(Default)]
pub(crate) struct Local {
    built: HashMap<usize, (Weak<GpuAllocation>, Vec<(usize, GpuImage)>)>,
    /// `half` passes recorded for minification (diagnostics, tests).
    pub(crate) passes: u64,
}

fn key(img: &GpuImage) -> Option<(usize, &Arc<GpuAllocation>)> {
    let a = img.allocation.as_ref()?;
    Some((Arc::as_ptr(a) as usize, a))
}

/// Remove the chains whose image is gone (released by the caller, outside the lock).
fn take_dead(chains: &mut HashMap<usize, Chain>) -> Vec<Chain> {
    let dead: Vec<usize> = chains.iter().filter(|(_, c)| c.source.strong_count() == 0).map(|(k, _)| *k).collect();
    dead.into_iter().filter_map(|k| chains.remove(&k)).collect()
}

impl Mips {
    /// `img` will not change while it lives: its levels may be kept.
    pub(crate) fn seal(&self, img: &GpuImage) {
        let Some((k, a)) = key(img) else { return };
        let source = Arc::downgrade(a);
        let dead = {
            let Ok(mut chains) = self.chains.lock() else { return };
            if chains.get(&k).is_some_and(|c| c.source.ptr_eq(&source)) {
                return;
            }
            let mut dead = if chains.len() >= MAX_CHAINS { take_dead(&mut chains) } else { Vec::new() };
            if chains.len() < MAX_CHAINS && chains.try_reserve(1).is_ok() {
                dead.extend(chains.insert(k, Chain { source, levels: Vec::new() }));
            }
            dead
        };
        dead.into_iter().for_each(Chain::release);
    }

    /// Whether `img` is sealed.
    fn sealed(&self, k: usize, a: &Arc<GpuAllocation>) -> bool {
        self.chains.lock().is_ok_and(|chains| chains.get(&k).is_some_and(|c| c.source.ptr_eq(&Arc::downgrade(a))))
    }

    /// Whether `img` (any clone of it) is sealed, so it must never be drawn on in place.
    pub(crate) fn is_sealed(&self, img: &GpuImage) -> bool {
        key(img).is_some_and(|(k, a)| self.sealed(k, a))
    }

    /// Published level `n` of a sealed image, while its slot is retained.
    fn level(&self, k: usize, a: &Arc<GpuAllocation>, n: usize, generation: &effectcraft_render::allocation::GpuGeneration) -> Option<GpuImage> {
        let (owner, slot) = {
            let chains = self.chains.lock().ok()?;
            let chain = chains.get(&k).filter(|c| c.source.ptr_eq(&Arc::downgrade(a)))?;
            let level = chain.levels.get(n.checked_sub(1)?)?.as_ref()?;
            (level.owner.clone(), level.slot.clone())
        };
        owner.get(&slot, generation, GpuStage::Blend)?.payload::<GpuImage>().cloned()
    }

    /// Drop every chain (their slots go back to their owners).
    pub(crate) fn clear(&self) {
        let old = self.chains.lock().ok().map(|mut c| std::mem::take(&mut *c));
        old.into_iter().flat_map(HashMap::into_values).for_each(Chain::release);
    }
}

/// `ops::warp_q`'s minification pre-filter for a mean scale below ½: `src` halved until the
/// remaining scale is at least ½ (or a side reaches one pixel), and the factor it shrank by.
/// Levels of a sealed image come from this encoder's own, then the published ones, and are
/// built (and kept) otherwise; the steps and kernels are those of building every level anew.
pub(crate) fn minify(e: &mut Enc, src: &GpuImage, scale: f64) -> (GpuImage, f64) {
    let sealed = e.reuse.then(|| key(src)).flatten().filter(|(k, a)| e.g.mips.sealed(*k, a)).map(|(k, a)| (k, a.clone()));
    let mut s = scale * 2.0;
    let mut f = 0.5;
    let mut n = 1;
    let mut img = level(e, sealed.as_ref(), n, src);
    while s < 0.5 && img.width > 1 && img.height > 1 {
        n += 1;
        img = level(e, sealed.as_ref(), n, &img);
        f *= 0.5;
        s *= 2.0;
    }
    (img, f)
}

/// Level `n` of an image (`prev`: its level `n - 1`; `sealed`: the image's key when sealed).
fn level(e: &mut Enc, sealed: Option<&(usize, Arc<GpuAllocation>)>, n: usize, prev: &GpuImage) -> GpuImage {
    let Some((k, a)) = sealed.filter(|_| n <= MAX_LEVELS) else {
        e.mips.passes = e.mips.passes.saturating_add(1);
        return crate::ops::half(e, prev);
    };
    let local = e.mips.built.get(k).filter(|(s, _)| s.ptr_eq(&Arc::downgrade(a))).and_then(|(_, levels)| levels.iter().find(|(m, _)| *m == n));
    if let Some((_, img)) = local {
        return img.clone();
    }
    if let Some(img) =
        e.g.mips.level(*k, a, n, &e.g.generation).filter(|l| (l.width, l.height) == (prev.width.div_ceil(2).max(1), prev.height.div_ceil(2).max(1)))
    {
        return img;
    }
    e.mips.passes = e.mips.passes.saturating_add(1);
    let img = crate::ops::half(e, prev);
    let entry = e.mips.built.entry(*k).or_insert_with(|| (Arc::downgrade(a), Vec::new()));
    if entry.0.ptr_eq(&Arc::downgrade(a)) {
        entry.1.push((n, img.clone()));
    }
    img
}

/// Publish the levels `e` built, once everything it recorded has been submitted (from
/// `Enc::publish_native`, after the submission and a health check): each becomes an owner slot
/// in the shared budget. Without an owner, or with work still unsubmitted, they are dropped.
pub(crate) fn publish(e: &mut Enc) {
    let built = std::mem::take(&mut e.mips.built);
    let g = e.g;
    let dead = g.mips.chains.lock().map(|mut c| take_dead(&mut c)).unwrap_or_default();
    dead.into_iter().for_each(Chain::release);
    let Some(owner) = e.owner.clone().filter(|o| o.valid()) else { return };
    if !e.submitted_all() || g.deferred.is_some() {
        return;
    }
    let cap = g.retained_cap();
    for (k, (source, levels)) in built {
        for (n, img) in levels {
            let Some(allocation) = img.allocation.clone().filter(|a| a.generation().same(&g.generation)) else { continue };
            img.share();
            g.leased.publish("mips", &img);
            let Ok(slot) = owner.prepare(Arc::new(img), allocation, GpuStage::Blend, cap).and_then(|p| p.publish()) else { continue };
            let level = Level { owner: owner.clone(), slot };
            let rejected = match g.mips.chains.lock() {
                Ok(mut chains) => match chains.get_mut(&k).filter(|c| c.source.ptr_eq(&source) && source.strong_count() > 0) {
                    Some(chain) => {
                        let i = n.saturating_sub(1);
                        if chain.levels.len() <= i {
                            chain.levels.resize_with(i.saturating_add(1), || None);
                        }
                        match chain.levels.get_mut(i) {
                            Some(slot) => slot.replace(level),
                            None => Some(level),
                        }
                    }
                    None => Some(level),
                },
                Err(_) => Some(level),
            };
            if let Some(old) = rejected {
                old.release();
            }
        }
    }
}
