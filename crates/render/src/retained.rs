//! Foundation for retained-content accounting; not yet wired into LayerCache.
//!
//! CPU buffers keep their existing Arc API. Weak records continue charging evicted
//! allocations held by callers, until a bounded sweep observes their last drop.
//! This does not account for uncached caller allocations or GPU storage yet.
//! Only concrete CPU Buf records are supported: retiring them drops CPU vectors
//! under the mutex. Before adding GPU or callback-bearing payloads, move resource
//! destruction outside this mutex; no backend work or callbacks belong here.
use effectcraft_effects::Buf;
use std::sync::{Arc, Mutex, PoisonError, Weak};

/// Hard ceiling bounds weak metadata and every sweep, even for hostile preferences.
pub const MAX_RECORDS: usize = 4096;

/// Admission failure leaves the caller's pixels untouched.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdmissionError {
    /// Byte allowance or allocation-record limit reached (possibly pinned by callers).
    Capacity,
    /// Input byte arithmetic overflowed.
    Overflow,
    /// Allocation of bounded accounting metadata failed.
    Allocation,
    /// A reservation predates retirement of this owner's contents.
    Retired,
    /// Actual pixels exceed the reserved byte count.
    TooSmall,
}

/// Snapshot includes pending reservations and externally pinned retired buffers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RetainedStats {
    pub live_bytes: usize,
    pub pending_bytes: usize,
    pub records: usize,
    pub pending_records: usize,
    pub retained_records: usize,
    pub debt_bytes: usize,
}

struct CpuRecord {
    weak: Weak<Buf>,
    retained: Option<Arc<Buf>>,
    bytes: usize,
}
struct State {
    records: Vec<CpuRecord>, // retained entries ordered oldest first
    bytes: usize,
    pending_bytes: usize,
    pending_records: usize,
    budget: usize,
    cap: usize,
    epoch: Arc<()>,
}
impl State {
    fn sweep(&mut self) {
        self.records.retain(|r| r.weak.strong_count() > 0);
        self.bytes = self.records.iter().fold(0usize, |sum, r| sum.saturating_add(r.bytes));
    }
    fn fits(&self, bytes: usize, count: usize) -> bool {
        self.bytes.checked_add(self.pending_bytes).and_then(|n| n.checked_add(bytes)).is_some_and(|n| n <= self.budget)
            && self.records.len().checked_add(self.pending_records).and_then(|n| n.checked_add(count)).is_some_and(|n| n <= self.cap)
    }
    fn relieve(&mut self, bytes: usize, count: usize) {
        self.sweep();
        while !self.fits(bytes, count) {
            let Some(record) = self.records.iter_mut().find(|r| r.retained.is_some()) else { break };
            record.retained = None;
            self.sweep();
        }
    }

    fn retain_cpu(&mut self, buf: &Arc<Buf>, bytes: usize) -> Result<(), AdmissionError> {
        if bytes > self.budget || self.cap == 0 { return Err(AdmissionError::Capacity); }
        self.sweep();
        if let Some(i) = self.records.iter().position(|r| r.weak.ptr_eq(&Arc::downgrade(buf))) {
            if !self.fits(0, 0) {
                return Err(AdmissionError::Capacity);
            }
            let mut r = self.records.remove(i);
            r.retained = Some(buf.clone());
            self.records.push(r);
            return Ok(());
        }
        self.relieve(bytes, 1);
        if !self.fits(bytes, 1) {
            return Err(AdmissionError::Capacity);
        }
        self.records.try_reserve_exact(1).map_err(|_| AdmissionError::Allocation)?;
        self.bytes = self.bytes.checked_add(bytes).ok_or(AdmissionError::Overflow)?;
        self.records.push(CpuRecord { weak: Arc::downgrade(buf), retained: Some(buf.clone()), bytes });
        Ok(())
    }

}

/// A bounded CPU ownership ledger. Existing caches are not implicitly enrolled.
#[derive(Clone)]
pub struct RetainedBudget(Arc<Mutex<State>>);
impl RetainedBudget {
    pub fn new(bytes: usize, records: usize) -> Self {
        Self(Arc::new(Mutex::new(State {
            records: Vec::new(),
            bytes: 0,
            pending_bytes: 0,
            pending_records: 0,
            budget: bytes,
            cap: records.min(MAX_RECORDS),
            epoch: Arc::new(()),
        })))
    }

    /// Reserve before producing a retained result. Dropping cancels the reservation.
    pub fn reserve(&self, bytes: usize) -> Result<Reservation, AdmissionError> {
        let mut s = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let pending_bytes = s.pending_bytes.checked_add(bytes).ok_or(AdmissionError::Overflow)?;
        let pending_records = s.pending_records.checked_add(1).ok_or(AdmissionError::Overflow)?;
        // Reject impossible requests without evicting useful contents.
        if bytes > s.budget || s.cap == 0 {
            return Err(AdmissionError::Capacity);
        }
        s.relieve(bytes, 1);
        if !s.fits(bytes, 1) {
            return Err(AdmissionError::Capacity);
        }
        // Include slots promised to other outstanding reservations.
        s.records.try_reserve_exact(pending_records).map_err(|_| AdmissionError::Allocation)?;
        s.pending_bytes = pending_bytes;
        s.pending_records = pending_records;
        Ok(Reservation { owner: self.clone(), bytes, epoch: s.epoch.clone(), active: true })
    }

    /// Retain existing CPU pixels, deduplicating Arc aliases and touching recency.
    /// Admission and publication share one critical section with retirement.
    /// Retired records can only be enrolled again through this explicit call;
    /// producers that begin before retirement must instead use a Reservation.
    pub fn retain_cpu(&self, buf: &Arc<Buf>) -> Result<(), AdmissionError> {
        let bytes = cpu_bytes(buf)?;
        let mut s = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        s.retain_cpu(buf, bytes)
    }

    /// Shrink immediately retires least-recently retained owners; callers may pin debt.
    pub fn set_budget(&self, bytes: usize) {
        let mut s = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        s.budget = bytes;
        s.relieve(0, 0);
    }

    /// Drop retained ownership and invalidate pending producers, keeping pinned charges.
    pub fn retire(&self) {
        let mut s = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        s.epoch = Arc::new(());
        for r in &mut s.records {
            r.retained = None;
        }
        s.sweep();
    }

    pub fn stats(&self) -> RetainedStats {
        let mut s = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        s.sweep();
        RetainedStats {
            live_bytes: s.bytes,
            pending_bytes: s.pending_bytes,
            records: s.records.len(),
            pending_records: s.pending_records,
            retained_records: s.records.iter().filter(|r| r.retained.is_some()).count(),
            debt_bytes: s.bytes.saturating_add(s.pending_bytes).saturating_sub(s.budget),
        }
    }
}
fn cpu_bytes(buf: &Buf) -> Result<usize, AdmissionError> {
    buf.img.data.capacity().checked_mul(std::mem::size_of::<effectcraft_raster::Px>()).ok_or(AdmissionError::Overflow)
}

/// One pending allocation slot; committing validates actual CPU bytes and retirement.
pub struct Reservation {
    owner: RetainedBudget,
    bytes: usize,
    epoch: Arc<()>,
    active: bool,
}
impl Reservation {
    pub fn commit_cpu(mut self, buf: &Arc<Buf>) -> Result<(), AdmissionError> {
        let bytes = cpu_bytes(buf)?;
        if bytes > self.bytes {
            return Err(AdmissionError::TooSmall);
        }
        let mut s = self.owner.0.lock().unwrap_or_else(PoisonError::into_inner);
        s.pending_bytes = s.pending_bytes.saturating_sub(self.bytes);
        s.pending_records = s.pending_records.saturating_sub(1);
        self.active = false;
        if !Arc::ptr_eq(&self.epoch, &s.epoch) {
            return Err(AdmissionError::Retired);
        }
        s.retain_cpu(buf, bytes)
    }
}
impl Drop for Reservation {
    fn drop(&mut self) {
        if self.active {
            let mut s = self.owner.0.lock().unwrap_or_else(PoisonError::into_inner);
            s.pending_bytes = s.pending_bytes.saturating_sub(self.bytes);
            s.pending_records = s.pending_records.saturating_sub(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn buf() -> Arc<Buf> {
        Arc::new(Buf { img: effectcraft_raster::Image::new(1, 1), offset: [0.0; 2], scale: 1.0 })
    }

    #[test]
    fn aliases_copies_and_pinned_retirement() {
        let ledger = RetainedBudget::new(64, 4);
        let a = buf();
        ledger.retain_cpu(&a.clone()).unwrap();
        ledger.retain_cpu(&a.clone()).unwrap();
        ledger.retain_cpu(&Arc::new((*a).clone())).unwrap();
        assert_eq!(ledger.stats().live_bytes, 32);
        ledger.retire();
        assert_eq!(ledger.stats().live_bytes, 16);
        assert_eq!(ledger.stats().retained_records, 0);
        drop(a);
        assert_eq!(ledger.stats().live_bytes, 0);
    }

    #[test]
    fn pending_cancellation_actual_size_and_overflow() {
        let ledger = RetainedBudget::new(usize::MAX, 3);
        let pending = ledger.reserve(usize::MAX).unwrap();
        assert!(matches!(ledger.reserve(1), Err(AdmissionError::Overflow)));
        drop(pending);
        ledger.reserve(32).unwrap().commit_cpu(&buf()).unwrap();
        assert_eq!(ledger.stats().live_bytes, 16);
        assert_eq!(ledger.stats().pending_bytes, 0);
        assert_eq!(ledger.reserve(1).unwrap().commit_cpu(&buf()), Err(AdmissionError::TooSmall));
        assert_eq!(ledger.stats().pending_records, 0);
    }

    #[test]
    fn shrink_keeps_pinned_debt_and_rejects_stale_publication() {
        let ledger = RetainedBudget::new(64, 4);
        let a = buf();
        ledger.retain_cpu(&a.clone()).unwrap();
        let pending = ledger.reserve(16).unwrap();
        ledger.set_budget(0);
        assert_eq!(ledger.stats().debt_bytes, 32);
        ledger.retire();
        assert_eq!(pending.commit_cpu(&buf()), Err(AdmissionError::Retired));
        assert_eq!(ledger.stats().debt_bytes, 16);
        drop(a);
        assert_eq!(ledger.stats().debt_bytes, 0);
    }

    #[test]
    fn record_cap_includes_pinned_and_pending_zero_byte_entries() {
        let ledger = RetainedBudget::new(128, 1);
        let p = ledger.reserve(0).unwrap();
        assert!(matches!(ledger.reserve(0), Err(AdmissionError::Capacity)));
        drop(p);
        let a = buf();
        ledger.retain_cpu(&a.clone()).unwrap();
        ledger.retire();
        assert_eq!(ledger.retain_cpu(&buf()), Err(AdmissionError::Capacity));
        assert_eq!(ledger.stats().records, 1);
        drop(a);
        ledger.retain_cpu(&buf()).unwrap();
    }

    #[test]
    fn recency_evicts_oldest_unpinned_owner() {
        let ledger = RetainedBudget::new(32, 3);
        let a = buf();
        let b = buf();
        let wa = Arc::downgrade(&a);
        let wb = Arc::downgrade(&b);
        ledger.retain_cpu(&a.clone()).unwrap();
        ledger.retain_cpu(&b.clone()).unwrap();
        ledger.retain_cpu(&a.clone()).unwrap();
        drop(a);
        drop(b);
        ledger.retain_cpu(&buf()).unwrap();
        assert!(wa.upgrade().is_some());
        assert!(wb.upgrade().is_none());
        assert_eq!(ledger.stats().live_bytes, 32);
    }
    #[test]
    fn capacity_not_length_is_charged() {
        let mut a = (*buf()).clone();
        a.img.data.reserve_exact(15);
        let bytes = a.img.data.capacity() * std::mem::size_of::<effectcraft_raster::Px>();
        assert!(bytes > 16);
        let ledger = RetainedBudget::new(bytes, 1);
        ledger.retain_cpu(&Arc::new(a)).unwrap();
        assert_eq!(ledger.stats().live_bytes, bytes);
    }

    #[test]
    fn independent_reservations_cannot_charge_another_owner() {
        let a = RetainedBudget::new(16, 1);
        let b = RetainedBudget::new(16, 1);
        let pending = a.reserve(16).unwrap();
        assert_eq!(a.stats().pending_bytes, 16);
        assert_eq!(b.stats(), RetainedStats::default());
        pending.commit_cpu(&buf()).unwrap();
        assert_eq!(a.stats().live_bytes, 16);
        assert_eq!(b.stats(), RetainedStats::default());
    }

    #[test]
    fn duplicate_concurrent_publications_charge_once() {
        let ledger = RetainedBudget::new(64, 4);
        let a = buf();
        let first = ledger.reserve(16).unwrap();
        let second = ledger.reserve(16).unwrap();
        let other = a.clone();
        let worker = std::thread::spawn(move || first.commit_cpu(&other));
        second.commit_cpu(&a).unwrap();
        worker.join().unwrap().unwrap();
        assert_eq!(ledger.stats().live_bytes, 16);
        assert_eq!(ledger.stats().records, 1);
        assert_eq!(ledger.stats().pending_records, 0);
    }

    #[test]
    fn dead_weak_slots_reclaim_capacity_without_aliasing_new_buffers() {
        let ledger = RetainedBudget::new(16, 1);
        for _ in 0..32 {
            let a = buf();
            let weak = Arc::downgrade(&a);
            ledger.retain_cpu(&a.clone()).unwrap();
            ledger.retire();
            drop(a);
            assert!(weak.upgrade().is_none());
            let replacement = buf();
            assert!(!weak.ptr_eq(&Arc::downgrade(&replacement)));
            ledger.retain_cpu(&replacement).unwrap();
            assert_eq!(ledger.stats().records, 1);
            ledger.retire();
        }
        assert_eq!(ledger.stats(), RetainedStats::default());
    }

    #[test]
    fn shrink_rechecks_pending_commit_without_retirement() {
        let ledger = RetainedBudget::new(32, 2);
        let pending = ledger.reserve(32).unwrap();
        ledger.set_budget(8);
        assert_eq!(pending.commit_cpu(&buf()), Err(AdmissionError::Capacity));
        assert_eq!(ledger.stats(), RetainedStats::default());
    }

    #[test]
    fn metadata_cap_is_hard_bounded() {
        let ledger = RetainedBudget::new(usize::MAX, usize::MAX);
        let pending: Vec<_> = (0..MAX_RECORDS).map(|_| ledger.reserve(0).unwrap()).collect();
        assert!(matches!(ledger.reserve(0), Err(AdmissionError::Capacity)));
        drop(pending);
        assert_eq!(ledger.stats(), RetainedStats::default());
    }
    #[test]
    fn retirement_rejects_waiting_producer_and_keeps_existing_caller_charge() {
        let ledger = RetainedBudget::new(64, 4);
        let a = buf();
        ledger.retain_cpu(&a.clone()).unwrap();
        let pending = ledger.reserve(16).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            rx.recv().unwrap();
            pending.commit_cpu(&buf())
        });
        ledger.retire();
        tx.send(()).unwrap();
        assert_eq!(worker.join().unwrap(), Err(AdmissionError::Retired));
        assert_eq!(ledger.stats().retained_records, 0);
        assert_eq!(ledger.stats().live_bytes, 16);
        // Re-admission is an explicit new operation and may share the live allocation.
        ledger.retain_cpu(&a).unwrap();
        assert_eq!(ledger.stats().retained_records, 1);
        assert_eq!(ledger.stats().live_bytes, 16);
    }

    #[test]
    fn denial_preserves_callers_only_cpu_fallback() {
        let a = buf();
        let ledger = RetainedBudget::new(0, 0);
        assert_eq!(ledger.retain_cpu(&a), Err(AdmissionError::Capacity));
        assert_eq!(Arc::strong_count(&a), 1);
        let ledger = RetainedBudget::new(16, 1);
        let pending = ledger.reserve(1).unwrap();
        assert_eq!(pending.commit_cpu(&a), Err(AdmissionError::TooSmall));
        assert_eq!(Arc::strong_count(&a), 1);
        assert_eq!(a.img.data.len(), 1);
        assert_eq!(ledger.stats(), RetainedStats::default());
    }

}
