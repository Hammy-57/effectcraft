//! Opaque GPU ownership. Payloads leave all locks before their destructors run.
use super::{Inner, ResidentToken};
use crate::allocation::{GpuAllocation, GpuGeneration};
use crate::retained::{AdmissionError, MAX_RECORDS, Reservation};
use std::any::Any;
use std::sync::{Arc, Mutex, Weak};

/// Exact representation stage; a CPU mirror is identified by its source Arc.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GpuStage {
    CpuMirror,
    Processed,
    Blend,
}

pub struct GpuSlot {
    payload: Arc<dyn Any + Send + Sync>,
    allocation: Arc<GpuAllocation>,
    stage: GpuStage,
}
impl GpuSlot {
    pub fn payload<T: Any>(&self) -> Option<&T> {
        self.payload.downcast_ref()
    }
    pub fn allocation(&self) -> &Arc<GpuAllocation> {
        &self.allocation
    }
    pub fn stage(&self) -> GpuStage {
        self.stage
    }
}
pub(super) struct GpuEntry {
    pub(super) slot: Arc<GpuSlot>,
    pub(super) last_use: u64,
}

/// A weak per-operation capability. Never install one globally in a shared backend.
#[derive(Clone)]
pub struct GpuOwner {
    inner: Weak<Mutex<Inner>>,
    token: ResidentToken,
}
impl GpuOwner {
    pub(super) fn new(inner: Weak<Mutex<Inner>>, token: ResidentToken) -> Self {
        Self { inner, token }
    }
    pub fn identity(&self) -> usize {
        self.token.identity()
    }
    pub fn valid(&self) -> bool {
        self.token.valid() && self.inner.strong_count() > 0
    }

    pub fn prepare(
        &self,
        payload: Arc<dyn Any + Send + Sync>,
        allocation: Arc<GpuAllocation>,
        stage: GpuStage,
        device_cap: usize,
    ) -> Result<GpuPending, AdmissionError> {
        if !allocation.generation().valid() {
            return Err(AdmissionError::Retired);
        }
        let inner = self.inner.upgrade().ok_or(AdmissionError::Retired)?;
        let (extra, claim) = {
            let g = inner.lock().map_err(|_| AdmissionError::Retired)?;
            if !self.valid() {
                return Err(AdmissionError::Retired);
            }
            if allocation.bytes() > g.budget || allocation.bytes() > device_cap {
                return Err(AdmissionError::Capacity);
            }
            let claim = g.physical.claim_gpu(&allocation)?;
            (if g.physical.tracks_gpu(&allocation) { 0 } else { allocation.bytes() }, claim)
        };
        relieve(&inner, Some(&self.token), extra, Some((allocation.generation(), device_cap)), true);
        let reservation = {
            let g = inner.lock().map_err(|_| AdmissionError::Retired)?;
            if !self.valid() {
                return Err(AdmissionError::Retired);
            }
            if g.map.len().saturating_add(g.gpu.len()) >= MAX_RECORDS {
                return Err(AdmissionError::Capacity);
            }
            if g.physical.gpu_bytes(allocation.generation()).checked_add(extra).is_none_or(|n| n > device_cap) {
                return Err(AdmissionError::Capacity);
            }
            g.physical.reserve_gpu(&allocation)?
        };
        let device = allocation.generation().reserve(&allocation, device_cap)?;
        Ok(GpuPending { device, claim, owner: self.clone(), reservation, slot: Arc::new(GpuSlot { payload, allocation, stage }) })
    }

    pub fn get(&self, weak: &Weak<GpuSlot>, generation: &GpuGeneration, stage: GpuStage) -> Option<Arc<GpuSlot>> {
        let slot = weak.upgrade()?; // declared before guards: drop always occurs after them
        if !generation.valid() || !slot.allocation.generation().same(generation) || slot.stage != stage {
            return None;
        }
        let inner = self.inner.upgrade()?;
        let mut g = inner.lock().ok()?;
        if !self.valid() {
            return None;
        }
        let id = Arc::as_ptr(&slot) as usize;
        if !g.gpu.get(&id).is_some_and(|e| Arc::ptr_eq(&e.slot, &slot)) {
            return None;
        }
        g.clock = g.clock.saturating_add(1);
        let now = g.clock;
        if let Some(e) = g.gpu.get_mut(&id) {
            e.last_use = now;
        }
        Some(slot)
    }

    pub fn set_device_cap(&self, generation: &GpuGeneration, cap: usize) {
        generation.set_cap(cap);
        self.enforce_device_cap(generation, cap);
    }
    pub fn enforce_device_cap(&self, generation: &GpuGeneration, cap: usize) {
        if let Some(inner) = self.inner.upgrade() {
            relieve(&inner, Some(&self.token), 0, Some((generation, cap)), false);
        }
    }
    pub fn release(&self, weak: &Weak<GpuSlot>) {
        let Some(inner) = self.inner.upgrade() else { return };
        let retired = {
            let Ok(mut g) = inner.lock() else { return };
            g.gpu.remove(&(weak.as_ptr() as usize)).map(|e| e.slot)
        };
        drop(retired);
    }

    /// Remove only this device incarnation; CPU allocations and other devices survive.
    pub fn retire_generation(&self, generation: &GpuGeneration) {
        generation.retire();
        let Some(inner) = self.inner.upgrade() else { return };
        loop {
            let retired = {
                let Ok(mut g) = inner.lock() else { return };
                let key = g.gpu.iter().find(|(_, e)| e.slot.allocation.generation().same(generation)).map(|(k, _)| *k);
                key.and_then(|k| g.gpu.remove(&k)).map(|e| e.slot)
            };
            if retired.is_none() {
                break;
            }
            drop(retired);
        }
    }
}

/// Reservation and transient payload awaiting a successful producer submission.
pub struct GpuPending {
    device: crate::allocation::DeviceReservation,
    claim: crate::allocation::AllocationClaim,
    owner: GpuOwner,
    reservation: Reservation,
    slot: Arc<GpuSlot>,
}
impl GpuPending {
    pub fn publish(self) -> Result<Weak<GpuSlot>, AdmissionError> {
        let inner = self.owner.inner.upgrade().ok_or(AdmissionError::Retired)?;
        let mut g = inner.lock().map_err(|_| AdmissionError::Retired)?;
        if !self.owner.valid() || !self.slot.allocation.generation().valid() {
            return Err(AdmissionError::Retired);
        }
        if g.map.len().saturating_add(g.gpu.len()) >= MAX_RECORDS {
            return Err(AdmissionError::Capacity);
        }
        g.gpu.try_reserve(1).map_err(|_| AdmissionError::Allocation)?;
        self.device.commit(self.reservation)?;
        self.claim.commit();
        g.clock = g.clock.saturating_add(1);
        let now = g.clock;
        let weak = Arc::downgrade(&self.slot);
        g.gpu.insert(Arc::as_ptr(&self.slot) as usize, GpuEntry { slot: self.slot, last_use: now });
        Ok(weak)
    }
}

/// A common recency eviction; each removed backend payload is destroyed after unlock.
pub(super) fn relieve(inner: &Arc<Mutex<Inner>>, token: Option<&ResidentToken>, extra: usize, device: Option<(&GpuGeneration, usize)>, slot: bool) {
    loop {
        let (progress, retired) = {
            let Ok(mut g) = inner.lock() else { return };
            if token.is_some_and(|t| !t.valid()) {
                return;
            }
            let stats = g.physical.stats();
            let bytes_fit = stats.live_bytes.checked_add(stats.pending_bytes).and_then(|n| n.checked_add(extra)).is_some_and(|n| n <= g.budget);
            let device_fit =
                device.is_none_or(|(generation, cap)| generation.within_cap() && g.physical.gpu_bytes(generation).checked_add(extra).is_some_and(|n| n <= cap));
            let count_fit = !slot || g.map.len().saturating_add(g.gpu.len()) < MAX_RECORDS;
            if bytes_fit && device_fit && count_fit {
                return;
            }
            super::evict_one(&mut g)
        };
        drop(retired);
        if !progress {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::LayerCache;
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Probe {
        inner: Weak<Mutex<Inner>>,
        drops: Arc<AtomicUsize>,
    }
    impl Drop for Probe {
        fn drop(&mut self) {
            if let Some(inner) = self.inner.upgrade() {
                let g = inner.try_lock().expect("backend drop outside owner lock");
                let _ = g.physical.stats();
            }
            self.drops.fetch_add(1, Ordering::Relaxed);
        }
    }
    fn allocation(g: &GpuGeneration) -> Arc<GpuAllocation> {
        GpuAllocation::rgba_f32(1, 1, g.clone()).unwrap()
    }
    fn probe(cache: &LayerCache, drops: &Arc<AtomicUsize>) -> Arc<dyn Any + Send + Sync> {
        Arc::new(Probe { inner: Arc::downgrade(&cache.inner), drops: drops.clone() })
    }
    #[test]
    fn gpu_alias_pins_and_clear_drop_after_unlock() {
        let c = LayerCache::new(64);
        let o = c.gpu_owner().unwrap();
        let g = GpuGeneration::default();
        let a = allocation(&g);
        let drops = Arc::new(AtomicUsize::new(0));
        let x = o.prepare(probe(&c, &drops), a.clone(), GpuStage::Blend, 64).unwrap().publish().unwrap();
        let y = o.prepare(probe(&c, &drops), a.clone(), GpuStage::Blend, 64).unwrap().publish().unwrap();
        assert_eq!(c.retained_stats().live_bytes, 16);
        let pin = o.get(&x, &g, GpuStage::Blend).unwrap();
        c.clear();
        assert_eq!(drops.load(Ordering::Relaxed), 1);
        assert!(o.get(&y, &g, GpuStage::Blend).is_none());
        assert_eq!(c.retained_stats().live_bytes, 16);
        drop(a);
        drop(pin);
        assert_eq!(drops.load(Ordering::Relaxed), 2);
        assert_eq!(c.retained_stats().live_bytes, 0);
    }
    #[test]
    fn gpu_canceled_and_stale_publication_release_outside_locks() {
        let c = LayerCache::new(64);
        let o = c.gpu_owner().unwrap();
        let g = GpuGeneration::default();
        let drops = Arc::new(AtomicUsize::new(0));
        let p = o.prepare(probe(&c, &drops), allocation(&g), GpuStage::Blend, 64).unwrap();
        assert_eq!(c.retained_stats().pending_bytes, 16);
        drop(p);
        assert_eq!(c.retained_stats().pending_bytes, 0);
        let p = o.prepare(probe(&c, &drops), allocation(&g), GpuStage::Blend, 64).unwrap();
        c.clear();
        assert!(matches!(p.publish(), Err(AdmissionError::Retired)));
        assert_eq!(drops.load(Ordering::Relaxed), 2);
        assert_eq!(c.retained_stats().pending_bytes, 0);
    }
    #[test]
    fn gpu_cross_owner_denial_preserves_existing_slots() {
        let a = LayerCache::new(16);
        let b = LayerCache::new(16);
        let g = GpuGeneration::default();
        let ao = a.gpu_owner().unwrap();
        let bo = b.gpu_owner().unwrap();
        let allocation = allocation(&g);
        let drops = Arc::new(AtomicUsize::new(0));
        ao.prepare(probe(&a, &drops), allocation.clone(), GpuStage::Blend, 32).unwrap().publish().unwrap();
        let old = bo.prepare(probe(&b, &drops), super::tests::allocation(&g), GpuStage::Blend, 32).unwrap().publish().unwrap();
        assert!(matches!(bo.prepare(probe(&b, &drops), allocation, GpuStage::Blend, 32), Err(AdmissionError::ForeignOwner)));
        assert!(bo.get(&old, &g, GpuStage::Blend).is_some());
    }
    #[test]
    fn gpu_eviction_shrink_and_generation_retirement_preserve_cpu() {
        let c = LayerCache::new(64);
        let o = c.gpu_owner().unwrap();
        let g = GpuGeneration::default();
        let drops = Arc::new(AtomicUsize::new(0));
        let cpu = Arc::new(crate::Buf { img: effectcraft_raster::Image::new(1, 1), offset: [0.0; 2], scale: 1.0 });
        c.insert(7, cpu.clone());
        let x = o.prepare(probe(&c, &drops), allocation(&g), GpuStage::Blend, 64).unwrap().publish().unwrap();
        o.retire_generation(&g);
        assert!(o.get(&x, &g, GpuStage::Blend).is_none());
        assert_eq!(drops.load(Ordering::Relaxed), 1);
        assert!(c.get(7).is_some());
        let g = GpuGeneration::default();
        let x = o.prepare(probe(&c, &drops), allocation(&g), GpuStage::Blend, 64).unwrap().publish().unwrap();
        let pin = o.get(&x, &g, GpuStage::Blend).unwrap();
        c.set_budget(0);
        assert_eq!(drops.load(Ordering::Relaxed), 1);
        assert_eq!(c.retained_stats().debt_bytes, 32);
        drop(pin);
        drop(cpu);
        assert_eq!(drops.load(Ordering::Relaxed), 2);
        assert_eq!(c.retained_stats().live_bytes, 0);
    }
    #[test]
    fn gpu_generation_rejects_pending_and_wrong_stage() {
        let c = LayerCache::new(64);
        let o = c.gpu_owner().unwrap();
        let g = GpuGeneration::default();
        let drops = Arc::new(AtomicUsize::new(0));
        let x = o.prepare(probe(&c, &drops), allocation(&g), GpuStage::Blend, 64).unwrap().publish().unwrap();
        assert!(o.get(&x, &g, GpuStage::Processed).is_none());
        assert!(o.get(&x, &GpuGeneration::default(), GpuStage::Blend).is_none());
        let pending = o.prepare(probe(&c, &drops), allocation(&g), GpuStage::Blend, 64).unwrap();
        o.retire_generation(&g);
        assert!(matches!(pending.publish(), Err(AdmissionError::Retired)));
        assert_eq!(drops.load(Ordering::Relaxed), 2);
    }
    #[test]
    fn gpu_shared_recency_and_pending_cap() {
        let c = LayerCache::new(32);
        let o = c.gpu_owner().unwrap();
        let g = GpuGeneration::default();
        let drops = Arc::new(AtomicUsize::new(0));
        let a = o.prepare(probe(&c, &drops), allocation(&g), GpuStage::Blend, 32).unwrap().publish().unwrap();
        let b = o.prepare(probe(&c, &drops), allocation(&g), GpuStage::Blend, 32).unwrap().publish().unwrap();
        drop(o.get(&a, &g, GpuStage::Blend).unwrap());
        let pending = o.prepare(probe(&c, &drops), allocation(&g), GpuStage::Blend, 32).unwrap();
        assert!(o.get(&b, &g, GpuStage::Blend).is_none());
        assert!(o.get(&a, &g, GpuStage::Blend).is_some());
        assert_eq!(c.retained_stats().pending_bytes, 16);
        pending.publish().unwrap();
        assert_eq!(c.retained_stats().live_bytes, 32);
    }
    #[test]
    fn gpu_pending_device_cap_and_canceled_owner_claim() {
        let a = LayerCache::new(128);
        let b = LayerCache::new(128);
        let ao = a.gpu_owner().unwrap();
        let bo = b.gpu_owner().unwrap();
        let g = GpuGeneration::default();
        let drops = Arc::new(AtomicUsize::new(0));
        let shared = GpuAllocation::rgba_f32(2, 1, g.clone()).unwrap();
        let old = bo.prepare(probe(&b, &drops), allocation(&g), GpuStage::Blend, 48).unwrap().publish().unwrap();
        let pending = ao.prepare(probe(&a, &drops), shared.clone(), GpuStage::Blend, 48).unwrap();
        assert!(matches!(bo.prepare(probe(&b, &drops), shared.clone(), GpuStage::Blend, 48), Err(AdmissionError::ForeignOwner)));
        assert!(bo.get(&old, &g, GpuStage::Blend).is_some());
        let second = GpuAllocation::rgba_f32(2, 1, g.clone()).unwrap();
        assert!(matches!(ao.prepare(probe(&a, &drops), second, GpuStage::Blend, 48), Err(AdmissionError::Capacity)));
        assert_eq!(a.retained_stats().pending_bytes, 32);
        drop(pending);
        assert_eq!(a.retained_stats().pending_bytes, 0);
        bo.prepare(probe(&b, &drops), shared, GpuStage::Blend, 48).unwrap().publish().unwrap();
    }
    #[test]
    fn gpu_global_ceiling_shrink_aliases_and_rollback() {
        let a = LayerCache::new(128);
        let b = LayerCache::new(128);
        let ao = a.gpu_owner().unwrap();
        let bo = b.gpu_owner().unwrap();
        let g = GpuGeneration::default();
        let drops = Arc::new(AtomicUsize::new(0));
        let image = GpuAllocation::rgba_f32(3, 1, g.clone()).unwrap();
        let p = ao.prepare(probe(&a, &drops), image.clone(), GpuStage::Blend, 64).unwrap();
        let alias = ao.prepare(probe(&a, &drops), image.clone(), GpuStage::Blend, 64).unwrap();
        assert_eq!(a.retained_stats().pending_bytes, 48);
        let other = GpuAllocation::rgba_f32(3, 1, g.clone()).unwrap();
        assert!(matches!(bo.prepare(probe(&b, &drops), other.clone(), GpuStage::Blend, 64), Err(AdmissionError::Capacity)));
        assert_eq!(b.retained_stats().pending_bytes, 0);
        drop(p);
        assert_eq!(a.retained_stats().pending_bytes, 48);
        alias.publish().unwrap();
        assert_eq!(a.retained_stats().live_bytes, 48);
        a.clear();
        drop(image);
        let pending = bo.prepare(probe(&b, &drops), other, GpuStage::Blend, 64).unwrap();
        bo.set_device_cap(&g, 16);
        assert!(matches!(pending.publish(), Err(AdmissionError::Capacity)));
        assert_eq!(b.retained_stats().pending_bytes, 0);
        assert!(g.within_cap());
        g.set_cap(64);
        let pending = bo.prepare(probe(&b, &drops), allocation(&g), GpuStage::Blend, 64).unwrap();
        b.clear();
        assert!(matches!(pending.publish(), Err(AdmissionError::Retired)));
        assert!(g.within_cap());
    }
    #[test]
    fn gpu_published_alias_discharges_pending_bytes_once() {
        for publish_alias in [false, true] {
            let c = LayerCache::new(64);
            let o = c.gpu_owner().unwrap();
            let g = GpuGeneration::default();
            let a = GpuAllocation::rgba_f32(3, 1, g.clone()).unwrap();
            let drops = Arc::new(AtomicUsize::new(0));
            let first = o.prepare(probe(&c, &drops), a.clone(), GpuStage::Blend, 64).unwrap();
            let second = o.prepare(probe(&c, &drops), a.clone(), GpuStage::Blend, 64).unwrap();
            assert_eq!(c.retained_stats().pending_bytes, 48);
            first.publish().unwrap();
            assert_eq!((c.retained_stats().live_bytes, c.retained_stats().pending_bytes), (48, 0));
            if publish_alias {
                second.publish().unwrap();
            } else {
                drop(second);
            }
            assert_eq!((c.retained_stats().live_bytes, c.retained_stats().pending_bytes), (48, 0));
            assert!(g.within_cap());
        }
    }
}
