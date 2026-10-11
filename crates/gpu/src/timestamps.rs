//! Opt-in per-encoder timestamp batches. At most eight batches may be in flight.
//! No timestamps are requested or allocated in ordinary builds.
use crate::context::GpuContext;
use std::sync::atomic::{AtomicUsize, Ordering};

const QUERIES: u32 = 1024;
static IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);
struct Permit;
impl Drop for Permit {
    fn drop(&mut self) {
        IN_FLIGHT.fetch_sub(1, Ordering::Relaxed);
    }
}
pub(crate) struct Batch {
    query: wgpu::QuerySet,
    names: Vec<String>,
    generation: u64,
    _permit: Permit,
}
impl Batch {
    pub(crate) fn new(g: &GpuContext) -> Option<Self> {
        let generation = effectcraft_render::profile::token()?;
        if !g.device.features().contains(wgpu::Features::TIMESTAMP_QUERY) {
            return None;
        }
        let mut count = IN_FLIGHT.load(Ordering::Relaxed);
        loop {
            if count >= 8 {
                effectcraft_render::profile::lost(generation);
                return None;
            }
            match IN_FLIGHT.compare_exchange_weak(count, count.saturating_add(1), Ordering::Relaxed, Ordering::Relaxed) {
                Ok(_) => break,
                Err(current) => count = current,
            }
        }
        let permit = Permit;
        let query = crate::context::init_resource(&g.device, "profile timestamps", || {
            g.device.create_query_set(&wgpu::QuerySetDescriptor { label: Some("profile timestamps"), ty: wgpu::QueryType::Timestamp, count: QUERIES })
        })
        .map_err(|e| {
            log::warn!("{e}");
            effectcraft_render::profile::lost(generation);
        })
        .ok()?;
        Some(Self { query, names: Vec::new(), generation, _permit: permit })
    }
    pub(crate) fn mark(&mut self, name: &str) -> Option<wgpu::ComputePassTimestampWrites<'_>> {
        let index = u32::try_from(self.names.len()).ok()?.checked_mul(2)?;
        if index.checked_add(1)? >= QUERIES || self.names.try_reserve(1).is_err() {
            effectcraft_render::profile::lost(self.generation);
            return None;
        }
        self.names.push(name.into());
        Some(wgpu::ComputePassTimestampWrites { query_set: &self.query, beginning_of_pass_write_index: Some(index), end_of_pass_write_index: Some(index + 1) })
    }
    pub(crate) fn resolve(self, g: &GpuContext, enc: &mut wgpu::CommandEncoder) -> Option<Pending> {
        let count = u32::try_from(self.names.len()).ok()?.checked_mul(2)?;
        if count == 0 {
            return None;
        }
        let size = u64::from(count).checked_mul(8)?;
        let resolve = crate::context::init_resource(&g.device, "profile resolve", || {
            g.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("profile resolve"),
                size,
                usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            })
        })
        .map_err(|e| {
            log::warn!("{e}");
            effectcraft_render::profile::lost(self.generation);
        })
        .ok()?;
        let read = crate::context::init_resource(&g.device, "profile read", || {
            g.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("profile read"),
                size,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            })
        })
        .map_err(|e| {
            log::warn!("{e}");
            effectcraft_render::profile::lost(self.generation);
        })
        .ok()?;
        enc.resolve_query_set(&self.query, 0..count, &resolve, 0);
        enc.copy_buffer_to_buffer(&resolve, 0, &read, 0, size);
        Some(Pending { batch: self, read })
    }
}
pub(crate) struct Pending {
    batch: Batch,
    read: wgpu::Buffer,
}
impl Pending {
    pub(crate) fn map(self, period: f32) {
        let read = self.read.clone();
        self.read.slice(..).map_async(wgpu::MapMode::Read, move |result| {
            if let Err(error) = result {
                log::warn!("GPU profile mapping: {error}");
                effectcraft_render::profile::lost(self.batch.generation);
                return;
            }
            {
                let Ok(bytes) = read.slice(..).get_mapped_range() else {
                    effectcraft_render::profile::lost(self.batch.generation);
                    read.unmap();
                    return;
                };
                for (name, pair) in self.batch.names.iter().zip(bytes.as_chunks::<16>().0.iter()) {
                    let Some(a) = pair.get(..8).and_then(|p| <[u8; 8]>::try_from(p).ok()) else { continue };
                    let Some(b) = pair.get(8..16).and_then(|p| <[u8; 8]>::try_from(p).ok()) else { continue };
                    // A reset/disjoint timestamp must not masquerade as a huge duration.
                    if let Some(ticks) = u64::from_le_bytes(b).checked_sub(u64::from_le_bytes(a)) {
                        effectcraft_render::profile::gpu(
                            self.batch.generation,
                            name,
                            ticks as f64 * f64::from(period) / 1000.0,
                            u64::from_le_bytes(a) as f64 * f64::from(period) / 1000.0,
                        );
                    } else {
                        effectcraft_render::profile::lost(self.batch.generation);
                    }
                }
            }
            read.unmap();
            drop(self.batch);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::{Enc, Params};
    #[test]
    fn timestamped_pass_preserves_pixels_and_reports_duration() {
        crate::tests::hold_gpu_lock();
        let gpu = crate::tests::gpu();
        assert!(gpu.is_some() || std::env::var("EFFECTCRAFT_REQUIRE_GPU").as_deref() != Ok("1"), "required GPU unavailable");
        let Some(gpu) = gpu else { return };
        let g = gpu.context();
        effectcraft_render::profile::start();
        let mut e = Enc::new(g);
        let src = e.image(4, 4);
        let out = e.image(4, 4);
        let mut params = Params::default();
        params.f[0] = [0.25; 4];
        e.pixels("fill", &params, &src, None, &out, None);
        let pixels = e.download(&out).expect("readback");
        g.device.poll(wgpu::PollType::Wait { submission_index: None, timeout: Some(std::time::Duration::from_secs(10)) }).expect("poll");
        let (events, dropped) = effectcraft_render::profile::finish();
        assert_eq!(pixels.data, vec![[0.25; 4]; 16]);
        assert_eq!(dropped, 0);
        if g.device.features().contains(wgpu::Features::TIMESTAMP_QUERY) {
            assert!(events.iter().any(|e| e.cat == "gpu" && e.name == "fill" && e.dur.is_finite() && e.dur >= 0.0));
        } else {
            assert!(events.iter().all(|e| e.cat != "gpu"));
        }
        assert_eq!(IN_FLIGHT.load(Ordering::Relaxed), 0);
    }
}
