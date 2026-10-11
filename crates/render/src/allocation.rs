//! Backend-neutral identities. No backend payload or callback is stored here.
use std::sync::{Arc, Mutex, PoisonError, Weak};

/// A context incarnation and its weak retained-allocation admission ceiling.
#[derive(Clone, Debug, Default)]
pub struct GpuGeneration(Arc<Generation>);
#[derive(Debug, Default)]
struct Generation {
    retired: std::sync::atomic::AtomicBool,
    budget: Mutex<DeviceBudget>,
}
#[derive(Debug)]
struct DeviceBudget {
    cap: usize,
    records: Vec<DeviceRecord>,
}
impl Default for DeviceBudget {
    fn default() -> Self {
        Self { cap: usize::MAX, records: Vec::new() }
    }
}
#[derive(Debug)]
struct DeviceRecord {
    allocation: Weak<GpuAllocation>,
    bytes: usize,
    pending: usize,
    committed: bool,
}
impl GpuGeneration {
    pub fn same(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
    pub fn identity(&self) -> usize {
        Arc::as_ptr(&self.0) as usize
    }
    pub fn valid(&self) -> bool {
        !self.0.retired.load(std::sync::atomic::Ordering::Acquire)
    }
    pub fn retire(&self) {
        self.0.retired.store(true, std::sync::atomic::Ordering::Release);
    }
    pub fn set_cap(&self, cap: usize) {
        self.0.budget.lock().unwrap_or_else(PoisonError::into_inner).cap = cap;
    }
    pub fn within_cap(&self) -> bool {
        let mut b = self.0.budget.lock().unwrap_or_else(PoisonError::into_inner);
        b.records.retain(|r| r.allocation.strong_count() > 0);
        b.records.iter().try_fold(0usize, |n, r| n.checked_add(r.bytes)).is_some_and(|n| n <= b.cap)
    }
    pub(crate) fn reserve(&self, allocation: &Arc<GpuAllocation>, cap: usize) -> Result<DeviceReservation, crate::retained::AdmissionError> {
        use crate::retained::{AdmissionError, MAX_RECORDS};
        let mut b = self.0.budget.lock().unwrap_or_else(PoisonError::into_inner);
        b.cap = b.cap.min(cap);
        b.records.retain(|r| r.allocation.strong_count() > 0);
        let weak = Arc::downgrade(allocation);
        let exists = b.records.iter().any(|r| r.allocation.ptr_eq(&weak));
        let extra = if exists { 0 } else { allocation.bytes };
        let total = b.records.iter().try_fold(extra, |n, r| n.checked_add(r.bytes)).ok_or(AdmissionError::Overflow)?;
        if !self.valid() {
            return Err(AdmissionError::Retired);
        }
        if total > b.cap.min(cap) {
            return Err(AdmissionError::Capacity);
        }
        if let Some(r) = b.records.iter_mut().find(|r| r.allocation.ptr_eq(&weak)) {
            r.pending = r.pending.checked_add(1).ok_or(AdmissionError::Overflow)?;
        } else {
            if b.records.len() >= MAX_RECORDS {
                return Err(AdmissionError::Capacity);
            }
            b.records.try_reserve(1).map_err(|_| AdmissionError::Allocation)?;
            b.records.push(DeviceRecord { allocation: weak, bytes: extra, pending: 1, committed: false });
        }
        Ok(DeviceReservation { generation: self.clone(), allocation: allocation.clone(), cap, active: true })
    }
}
pub(crate) struct DeviceReservation {
    generation: GpuGeneration,
    allocation: Arc<GpuAllocation>,
    cap: usize,
    active: bool,
}
impl DeviceReservation {
    pub(crate) fn commit(mut self, reservation: crate::retained::Reservation) -> Result<(), crate::retained::AdmissionError> {
        use crate::retained::AdmissionError;
        let mut b = self.generation.0.budget.lock().unwrap_or_else(PoisonError::into_inner);
        b.records.retain(|r| r.allocation.strong_count() > 0);
        let total = b.records.iter().try_fold(0usize, |n, r| n.checked_add(r.bytes)).ok_or(AdmissionError::Overflow)?;
        if !self.generation.valid() {
            return Err(AdmissionError::Retired);
        }
        if total > b.cap.min(self.cap) {
            return Err(AdmissionError::Capacity);
        }
        reservation.commit_gpu(&self.allocation, self.cap)?;
        let r = b.records.iter_mut().find(|r| r.allocation.ptr_eq(&Arc::downgrade(&self.allocation))).ok_or(AdmissionError::Retired)?;
        r.pending = r.pending.saturating_sub(1);
        r.committed = true;
        self.active = false;
        Ok(())
    }
}
impl Drop for DeviceReservation {
    fn drop(&mut self) {
        if self.active {
            let mut b = self.generation.0.budget.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(r) = b.records.iter_mut().find(|r| r.allocation.ptr_eq(&Arc::downgrade(&self.allocation))) {
                r.pending = r.pending.saturating_sub(1);
            }
            b.records.retain(|r| r.committed || r.pending > 0);
        }
    }
}

/// Created before any image clone escapes; every clone shares this allocation.
#[derive(Debug)]
pub struct GpuAllocation {
    bytes: usize,
    generation: GpuGeneration,
    enrolled: Mutex<Enrollment>,
}
impl GpuAllocation {
    pub fn rgba_f32(width: u32, height: u32, generation: GpuGeneration) -> Option<Arc<Self>> {
        let bytes = (width as usize).checked_mul(height as usize)?.checked_mul(16)?;
        Some(Arc::new(Self { bytes, generation, enrolled: Mutex::new(Enrollment::default()) }))
    }
    pub fn bytes(&self) -> usize {
        self.bytes
    }
    pub fn generation(&self) -> &GpuGeneration {
        &self.generation
    }
    pub(crate) fn claim(self: &Arc<Self>, owner: &Arc<()>) -> Option<AllocationClaim> {
        let mut e = self.enrolled.lock().unwrap_or_else(PoisonError::into_inner);
        if e.owner.strong_count() > 0 && !e.owner.ptr_eq(&Arc::downgrade(owner)) {
            return None;
        }
        if e.owner.strong_count() == 0 {
            *e = Enrollment { owner: Arc::downgrade(owner), ..Default::default() };
        }
        e.pending = e.pending.checked_add(1)?;
        Some(AllocationClaim { allocation: self.clone(), active: true })
    }
    pub(crate) fn accepts(&self, owner: &Arc<()>) -> bool {
        let e = self.enrolled.lock().unwrap_or_else(PoisonError::into_inner);
        e.owner.ptr_eq(&Arc::downgrade(owner))
    }
}
#[derive(Debug, Default)]
struct Enrollment {
    owner: Weak<()>,
    pending: usize,
    committed: bool,
}
pub(crate) struct AllocationClaim {
    allocation: Arc<GpuAllocation>,
    active: bool,
}
impl AllocationClaim {
    pub(crate) fn commit(mut self) {
        let mut e = self.allocation.enrolled.lock().unwrap_or_else(PoisonError::into_inner);
        e.committed = true;
        e.pending = e.pending.saturating_sub(1);
        self.active = false;
    }
}
impl Drop for AllocationClaim {
    fn drop(&mut self) {
        if self.active {
            let mut e = self.allocation.enrolled.lock().unwrap_or_else(PoisonError::into_inner);
            e.pending = e.pending.saturating_sub(1);
            if e.pending == 0 && !e.committed {
                e.owner = Weak::new();
            }
        }
    }
}
