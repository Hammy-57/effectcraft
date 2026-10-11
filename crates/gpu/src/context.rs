//! Device, pipelines, textures, uploads and readback.

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex, Weak};

use effectcraft_effects::Buf;
use effectcraft_raster::Image;
use effectcraft_render::cache::{GpuOwner, GpuPending, GpuSlot, GpuStage};

/// Compute entry points in `kernels.wgsl` (one pipeline each).
const ENTRIES: &[&str] = &[
    "warp_blend",
    "blend_full",
    "matte",
    "preserve",
    "knockout",
    "channel_mix",
    "quantize",
    "convert",
    "half",
    "adjust_mix",
    "bokeh_boost",
    "bokeh_prefix",
    "bokeh_gather",
    "fill",
];

/// Effect entry points in `kernels.wgsl`. Like the `fx_*` kernels, one that fails to build
/// (a driver's shader compiler rejecting it, #613) leaves its effects to the CPU instead of
/// turning the whole compositor off.
const EFFECT_ENTRIES: &[&str] = &["box_h", "box_v", "directional", "glow_bright", "glow_combine", "glow_ab", "shadow_make", "shadow_combine", "pointwise"];

/// Entry points that also bind group 1 (four read-only storage buffers; see
/// [`Enc::dispatch_ext`]).
const EXT_ENTRIES: &[&str] = &["classic3d"];

/// Pixel format of every working texture: premultiplied RGBA, 32-bit float (32 bpc headroom;
/// 8/16 bpc are emulated by clamping and quantising, as on the CPU).
pub const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba32Float;

/// Uploaded layer buffers kept on the GPU (bytes), at most (see [`GpuContext::set_cache_budget`]).
const UPLOAD_BUDGET: usize = 1536 << 20;

/// Effect chain results awaiting adoption (see [`GpuContext::keep`]), at most.
const MAX_KEPT: usize = 16;

/// Working textures kept for reuse once their last image is dropped (bytes), at most.
const POOL_BUDGET: usize = 768 << 20;

/// Small auxiliary caches must be bounded across sizes, not independently per size.
const AUX_CACHE_BUDGET: u64 = 64 << 20;

/// A GPU image: premultiplied RGBA f32 in a texture (cheap to clone; immutable once written).
/// Working images come from the context's texture pool and go back to it when the last clone
/// is dropped.
#[derive(Clone, Debug)]
pub struct GpuImage {
    pub texture: wgpu::Texture,
    pub width: u32,
    pub height: u32,
    /// The texture's default view, made once (kernels bind it on every dispatch).
    pub(crate) view: wgpu::TextureView,
    _lease: Option<Arc<Lease>>,
    /// Already clamped and quantised to this many levels (8/16 bpc), so quantising again is a
    /// no-op (see `ops::quantize`).
    pub(crate) levels: Option<f32>,
    // Last field: resource handles/lease are dropped before the accounting pin.
    pub(crate) allocation: Option<Arc<effectcraft_render::allocation::GpuAllocation>>,
}

impl GpuImage {
    /// An image on a texture of its own (not pooled).
    pub fn new(texture: wgpu::Texture, width: u32, height: u32) -> GpuImage {
        Self::in_generation(texture, width, height, effectcraft_render::allocation::GpuGeneration::default())
    }
    fn in_generation(texture: wgpu::Texture, width: u32, height: u32, generation: effectcraft_render::allocation::GpuGeneration) -> GpuImage {
        let allocation = (texture.format() == FORMAT && texture.mip_level_count() == 1 && texture.sample_count() == 1 && texture.depth_or_array_layers() == 1)
            .then(|| effectcraft_render::allocation::GpuAllocation::rgba_f32(texture.width(), texture.height(), generation))
            .flatten();
        let view = texture.create_view(&Default::default());
        GpuImage { allocation, texture, width, height, view, _lease: None, levels: None }
    }
}

/// Hands a pooled texture back to its [`Pool`] when the last [`GpuImage`] holding it drops.
struct Lease {
    texture: wgpu::Texture,
    pool: Weak<Mutex<Pool>>,
}

impl std::fmt::Debug for Lease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Lease")
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        if let Some(pool) = self.pool.upgrade()
            && let Ok(mut p) = pool.lock()
        {
            p.release(self.texture.clone());
        }
    }
}

/// Recycled working textures: allocating (and zero-initialising) a 1080p RGBA f32 texture
/// costs more than compositing a small layer into it, and a frame makes several per layer.
///
/// A released texture may still be read by commands recorded in an encoder that has not been
/// submitted yet. Reusing it in another encoder that is submitted first would overwrite it
/// before those reads run, so a released texture waits until every encoder that was open when
/// it was released has been submitted (or dropped).
#[derive(Default)]
pub(crate) struct Pool {
    free: HashMap<(u32, u32), Vec<wgpu::Texture>>,
    /// (texture, newest encoder open at its release).
    released: Vec<(wgpu::Texture, u64)>,
    released_bytes: usize,
    /// Encoders with recorded, unsubmitted work.
    open: BTreeSet<u64>,
    next: u64,
    bytes: usize,
    /// Byte budget of the free textures (`None`: [`POOL_BUDGET`]).
    budget: Option<usize>,
    /// Working textures reused / newly allocated (diagnostics, tests).
    hits: u64,
    misses: u64,
}

/// Shared-memory adapters retain fewer duplicate pixels: CPU caches already hold the sources.
fn cache_budgets(layer_cache: usize, integrated: bool) -> (usize, usize, u64) {
    let upload_cap = if integrated { 256 << 20 } else { UPLOAD_BUDGET };
    let pool_cap = if integrated { 128 << 20 } else { POOL_BUDGET };
    // Keep room for one 1080p RGBA-f32 buffer even on shared memory.
    let auxiliary_cap = if integrated { 32 << 20 } else { AUX_CACHE_BUDGET };
    (layer_cache.min(upload_cap), (layer_cache / 2).min(pool_cap), ((layer_cache / 16) as u64).min(auxiliary_cap))
}

/// Counts of uploads (CPU → GPU) and readbacks (GPU → CPU).
#[derive(Default)]
pub(crate) struct Transfers {
    ups: std::sync::atomic::AtomicU64,
    up_bytes: std::sync::atomic::AtomicU64,
    downs: std::sync::atomic::AtomicU64,
    down_bytes: std::sync::atomic::AtomicU64,
    yuv_conversions: std::sync::atomic::AtomicU64,
    yuv_bytes: std::sync::atomic::AtomicU64,
}

/// A snapshot of [`GpuContext::transfer_stats`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TransferStats {
    pub uploads: u64,
    pub upload_bytes: u64,
    pub readbacks: u64,
    pub readback_bytes: u64,
    /// Native plane conversion dispatches and their input sample bytes.
    pub yuv_conversions: u64,
    pub yuv_bytes: u64,
}

impl Transfers {
    pub(crate) fn count_yuv(&self, bytes: usize) {
        use std::sync::atomic::Ordering::Relaxed;
        self.yuv_conversions.fetch_add(1, Relaxed);
        self.yuv_bytes.fetch_add(bytes as u64, Relaxed);
    }

    pub(crate) fn count(&self, up: bool, bytes: usize) {
        use std::sync::atomic::Ordering::Relaxed;
        let (n, b) = if up { (&self.ups, &self.up_bytes) } else { (&self.downs, &self.down_bytes) };
        n.fetch_add(1, Relaxed);
        b.fetch_add(bytes as u64, Relaxed);
    }
}

fn tex_bytes(t: &wgpu::Texture) -> usize {
    (t.width() as usize).saturating_mul(t.height() as usize).saturating_mul(16)
}

impl Pool {
    fn release(&mut self, t: wgpu::Texture) {
        match self.open.last() {
            Some(&newest) => {
                let bytes = tex_bytes(&t);
                if self.bytes.saturating_add(self.released_bytes).saturating_add(bytes) <= self.budget.unwrap_or(POOL_BUDGET) {
                    self.released.push((t, newest));
                    self.released_bytes += bytes;
                }
                // Dropping our handle is safe: the encoder retains resources it still uses.
            }
            None => self.make_free(t),
        }
    }

    fn make_free(&mut self, t: wgpu::Texture) {
        let b = tex_bytes(&t);
        if self.bytes.saturating_add(self.released_bytes).saturating_add(b) > self.budget.unwrap_or(POOL_BUDGET) {
            return;
        }
        self.bytes += b;
        self.free.entry((t.width(), t.height())).or_default().push(t);
    }

    fn open(&mut self) -> u64 {
        self.next += 1;
        self.open.insert(self.next);
        self.next
    }

    /// Encoder `id` was submitted or dropped: free what no open encoder can still read.
    fn close(&mut self, id: u64) {
        self.open.remove(&id);
        let oldest = self.open.first().copied().unwrap_or(u64::MAX);
        if self.released.iter().all(|(_, w)| *w >= oldest) {
            return;
        }
        let (ready, wait): (Vec<_>, Vec<_>) = std::mem::take(&mut self.released).into_iter().partition(|(_, w)| *w < oldest);
        self.released = wait;
        self.released_bytes = self.released.iter().map(|(t, _)| tex_bytes(t)).sum();
        for (t, _) in ready {
            self.make_free(t);
        }
    }

    /// A last CPU lease released within the sole open encoder can be reused by
    /// later commands in that encoder. Separate encoders retain the submit barrier.
    fn take_for_encoder(&mut self, w: u32, h: u32, encoder: Option<u64>) -> Option<wgpu::Texture> {
        if let Some(id) = encoder.filter(|id| *id != 0)
            && self.open.len() == 1
            && self.open.contains(&id)
            && let Some(index) = self.released.iter().position(|(texture, owner)| *owner == id && texture.width() == w && texture.height() == h)
        {
            let (texture, _) = self.released.swap_remove(index);
            self.released_bytes = self.released_bytes.saturating_sub(tex_bytes(&texture));
            self.hits = self.hits.saturating_add(1);
            return Some(texture);
        }
        self.take(w, h)
    }

    fn take(&mut self, w: u32, h: u32) -> Option<wgpu::Texture> {
        let t = self.free.get_mut(&(w, h)).and_then(Vec::pop);
        if self.free.get(&(w, h)).is_some_and(Vec::is_empty) {
            self.free.remove(&(w, h));
        }
        match &t {
            Some(t) => {
                self.bytes -= tex_bytes(t);
                self.hits += 1;
            }
            None => {
                self.misses += 1;
                // A cached large texture must not permanently prevent reuse at a
                // smaller preview size. Evict only a free size that cannot coexist
                // with the requested texture at all. Preserve useful mixed smaller
                // sizes when their collective occupancy reaches the budget.
                let required = (w as usize).saturating_mul(h as usize).saturating_mul(16);
                let budget = self.budget.unwrap_or(POOL_BUDGET);
                if required <= budget {
                    while self.bytes.saturating_add(self.released_bytes).saturating_add(required) > budget {
                        let Some(key) = self
                            .free
                            .keys()
                            .filter(|(w, h)| (*w as usize).saturating_mul(*h as usize).saturating_mul(16).saturating_add(required) > budget)
                            .max_by_key(|(w, h)| u64::from(*w) * u64::from(*h))
                            .copied()
                        else {
                            break;
                        };
                        if let Some(texture) = self.free.get_mut(&key).and_then(Vec::pop) {
                            self.bytes = self.bytes.saturating_sub(tex_bytes(&texture));
                        }
                        if self.free.get(&key).is_some_and(Vec::is_empty) {
                            self.free.remove(&key);
                        }
                    }
                }
            }
        }
        t
    }

    fn clear(&mut self) {
        self.free.clear();
        self.released.clear();
        self.released_bytes = 0;
        self.bytes = 0;
    }

    /// Bound free and pending-reuse textures together (drop excess handles now).
    fn set_budget(&mut self, budget: usize) {
        self.budget = Some(budget);
        while self.bytes.saturating_add(self.released_bytes) > budget {
            let Some((texture, _)) = self.released.pop() else { break };
            self.released_bytes = self.released_bytes.saturating_sub(tex_bytes(&texture));
        }
        for list in self.free.values_mut() {
            while self.bytes > budget {
                let Some(t) = list.pop() else { break };
                self.bytes = self.bytes.saturating_sub(tex_bytes(&t));
            }
        }
        self.free.retain(|_, l| !l.is_empty());
    }
}

/// Bytes of a [`Params`] block.
const PARAMS_SIZE: u64 = 256;

/// [`Params`] blocks per uniform ring: one per dispatch until [`Enc::dispatch_ext`] submits.
const RING_SLOTS: usize = 256;

/// Uniform rings kept for reuse once their [`Enc`] is dropped, at most.
const RINGS_KEPT: usize = 8;

/// Uniform parameter block shared by every kernel (see `shaders/common.wgsl`).
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Params {
    pub u: [[u32; 4]; 4],
    pub f: [[f32; 4]; 12],
}

impl Params {
    fn bytes(&self) -> Vec<u8> {
        let mut v = Vec::with_capacity(256);
        for r in &self.u {
            for x in r {
                v.extend_from_slice(&x.to_le_bytes());
            }
        }
        for r in &self.f {
            for x in r {
                v.extend_from_slice(&x.to_le_bytes());
            }
        }
        v
    }
}

#[derive(Clone)]
struct SlotRef {
    owner: GpuOwner,
    slot: Weak<GpuSlot>,
    epoch: Weak<()>,
}
impl SlotRef {
    fn release(self) {
        self.owner.release(&self.slot);
    }
    fn get<T: Clone + 'static>(&self, generation: &effectcraft_render::allocation::GpuGeneration, stage: GpuStage) -> Option<T> {
        self.owner.get(&self.slot, generation, stage)?.payload::<T>().cloned()
    }
    fn alive(&self) -> bool {
        self.owner.valid() && self.epoch.strong_count() > 0 && self.slot.strong_count() > 0
    }
}
#[derive(Clone)]
struct Upload {
    buf: Weak<Buf>,
    slot: SlotRef,
}
#[derive(Clone)]
pub(crate) struct NativeImage {
    pub img: GpuImage,
    pub offset: [f64; 2],
    pub scale: f64,
}
#[derive(Clone)]
struct NativeUpload {
    owner: Weak<dyn effectcraft_render::footage::FramePixels>,
    token: effectcraft_render::cache::ResidentToken,
    metadata: Vec<u64>,
    slot: SlotRef,
}
#[derive(Default)]
struct Uploads {
    map: HashMap<(usize, usize), Upload>,
    native: HashMap<(usize, usize, u64), NativeUpload>,
    /// Per-device ceiling; the LayerCache owns the authoritative shared budget.
    budget: Option<usize>,
    kept: Vec<(u64, SlotRef)>,
    resident: Vec<SlotRef>,
    next_ticket: u64,
    keep_off: bool,
    epoch: Arc<()>,
}
impl Uploads {
    /// Only weak metadata leaves this lock. Release its owner slot afterwards.
    fn stale(&mut self) -> Option<SlotRef> {
        if let Some(k) = self.map.iter().find(|(_, e)| e.buf.strong_count() == 0 || !e.slot.alive()).map(|(k, _)| *k) {
            return self.map.remove(&k).map(|e| e.slot);
        }
        if let Some(k) = self.native.iter().find(|(_, e)| e.owner.strong_count() == 0 || !e.token.valid() || !e.slot.alive()).map(|(k, _)| *k) {
            return self.native.remove(&k).map(|e| e.slot);
        }
        if let Some(i) = self.kept.iter().position(|(_, e)| !e.alive()) {
            return Some(self.kept.remove(i).1);
        }
        if let Some(i) = self.resident.iter().position(|e| !e.alive()) {
            return Some(self.resident.remove(i));
        }
        None
    }
}

/// Drain weak indexes and release every scoped owner after the index lock.
fn clear_retained(uploads: &Mutex<Uploads>) {
    let old = if let Ok(mut u) = uploads.lock() {
        u.epoch = Arc::new(());
        Some((std::mem::take(&mut u.map), std::mem::take(&mut u.native), std::mem::take(&mut u.kept), std::mem::take(&mut u.resident)))
    } else {
        None
    };
    if let Some((map, native, kept, resident)) = old {
        for (_, e) in map {
            e.slot.release();
        }
        for (_, e) in native {
            e.slot.release();
        }
        for e in resident {
            e.release();
        }
        for (_, e) in kept {
            e.release();
        }
    }
}

/// The device and everything compiled for it.
pub struct GpuContext {
    generation: effectcraft_render::allocation::GpuGeneration,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub(crate) name: String,
    pub(crate) max_dim: u32,
    integrated: bool,
    auxiliary_budget: std::sync::atomic::AtomicU64,
    pub(crate) bgl: wgpu::BindGroupLayout,
    bgl_ext: wgpu::BindGroupLayout,
    pub(crate) fusion: crate::fusion::Pipelines,
    pub(crate) yuv: Option<crate::yuv::Pipelines>,
    pipelines: HashMap<&'static str, wgpu::ComputePipeline>,
    /// Effect kernels that failed to build, and why (their effects render on the CPU).
    unavailable: Vec<(&'static str, String)>,
    display_bgl: wgpu::BindGroupLayout,
    display: wgpu::ComputePipeline,
    /// The display texture's next mip level from the one above (`display.wgsl` `mip`).
    display_mip: wgpu::ComputePipeline,
    /// A 1 × 1 image bound where a kernel reads no image.
    dummy: GpuImage,
    dummy_buf: wgpu::Buffer,
    /// Bytes between the [`Params`] blocks of a uniform ring ([`Enc::dispatch_ext`]).
    uniform_stride: u64,
    /// Uniform rings no [`Enc`] holds (reused: see [`Enc::dispatch_ext`]).
    uniforms: Mutex<Vec<wgpu::Buffer>>,
    uploads: Arc<Mutex<Uploads>>,
    pool: Arc<Mutex<Pool>>,
    /// CPU ↔ GPU traffic (diagnostics: `bench --gpu`).
    pub(crate) transfers: Transfers,
    /// Readback staging buffers by size.
    staging: Mutex<HashMap<u64, Vec<wgpu::Buffer>>>,
    /// Transparent read-only images by size (see [`Enc::zeros`]).
    zeros: Mutex<HashMap<(u32, u32), GpuImage>>,
    /// Advanced 3D render pipelines (built on first use).
    pub(crate) adv3d: std::sync::OnceLock<Result<crate::adv3d::Pipes, String>>,
    pub(crate) static_pipeline_count: std::sync::atomic::AtomicU64,
    /// The adapter can run the Advanced 3D rasteriser (`adv3d::raster_unsupported`); without
    /// it Advanced 3D scenes render on the CPU.
    pub(crate) adv3d_raster: bool,
    /// GPU particle pipeline (built on first use) and simulation checkpoints.
    pub(crate) particles: crate::particles::PipesCell,
    pub(crate) particle_states: crate::particles::StatesCell,
    /// Deferred readbacks (a browser worker's WebGPU device), see [`crate::deferred`].
    pub(crate) deferred: Option<Arc<crate::deferred::Deferred>>,
    readbacks: Arc<crate::readback::Readbacks>,
}

fn tex_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: false },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

fn storage_tex_entry(binding: u32, format: wgpu::TextureFormat) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::StorageTexture { access: wgpu::StorageTextureAccess::WriteOnly, format, view_dimension: wgpu::TextureViewDimension::D2 },
        count: None,
    }
}

/// Contain synchronous native initialization errors without replacing the device owner's
/// handlers. Scopes are thread-local with wgpu's enabled `std` feature. The pinned native
/// wgpu-core backend returns an immediately-ready future from pop_error_scope, so these
/// drains do not poll or wait for unrelated submitted GPU work.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn init_resource<T>(device: &wgpu::Device, label: &str, create: impl FnOnce() -> T) -> Result<T, String> {
    let oom = device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
    let internal = device.push_error_scope(wgpu::ErrorFilter::Internal);
    let validation = device.push_error_scope(wgpu::ErrorFilter::Validation);
    // Some native drivers/wgpu paths can still unwind instead of reporting a scoped error.
    // Keep the last-resort guard inside the scopes, then drain every scope even on failure.
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(create));
    let validation_error = pollster::block_on(validation.pop());
    let internal_error = pollster::block_on(internal.pop());
    let oom_error = pollster::block_on(oom.pop());
    if let Some(error) = validation_error.or(internal_error).or(oom_error) {
        return Err(format!("GPU initialization ({label}): {error}"));
    }
    result.map_err(|payload| {
        let reason =
            payload.downcast_ref::<String>().map(String::as_str).or_else(|| payload.downcast_ref::<&str>().copied()).unwrap_or("native backend panicked");
        format!("GPU initialization ({label}): {reason}")
    })
}

/// Browser scope completion is asynchronous. Preserve the existing browser constructor;
/// never block its event loop or introduce a guard across an asynchronous boundary here.
#[cfg(target_arch = "wasm32")]
pub(crate) fn init_resource<T>(_device: &wgpu::Device, _label: &str, create: impl FnOnce() -> T) -> Result<T, String> {
    Ok(create())
}

/// A buffer holding `contents` (padded to 4 bytes), written through the queue. Unlike
/// `DeviceExt::create_buffer_init`, which maps the buffer at creation and panics when the
/// allocation failed (a driver reports it as an internal error, not out-of-memory: Adreno's
/// `ERROR_INITIALIZATION_FAILED`, #591), a failed allocation only yields an invalid buffer and
/// an error for the caller's error scope.
pub(crate) fn buffer_init(device: &wgpu::Device, queue: &wgpu::Queue, label: Option<&str>, contents: &[u8], usage: wgpu::BufferUsages) -> wgpu::Buffer {
    let align = wgpu::COPY_BUFFER_ALIGNMENT as usize;
    let padded = contents.len().div_ceil(align).saturating_mul(align);
    let buffer =
        device.create_buffer(&wgpu::BufferDescriptor { label, size: padded as u64, usage: usage | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
    if padded > 0 {
        if padded == contents.len() {
            queue.write_buffer(&buffer, 0, contents);
        } else {
            let mut data = contents.to_vec();
            data.resize(padded, 0);
            queue.write_buffer(&buffer, 0, &data);
        }
    }
    buffer
}

impl GpuContext {
    /// [`buffer_init`] on this context's device.
    pub(crate) fn buffer_init(&self, label: Option<&str>, contents: &[u8], usage: wgpu::BufferUsages) -> wgpu::Buffer {
        buffer_init(&self.device, &self.queue, label, contents, usage)
    }

    /// Build on an existing device (the desktop app shares egui-wgpu's). `Err` when the
    /// adapter cannot run the compositor (no compute shaders, e.g. WebGL2; no float storage
    /// textures) or the device was created with limits below what its pipelines bind.
    /// Preserves the host's device handlers. The host must forward failures using
    /// [`Self::device_failure_notifier`] for immediate readback cancellation.
    pub fn new(adapter: &wgpu::Adapter, device: wgpu::Device, queue: wgpu::Queue) -> Result<GpuContext, String> {
        let info = adapter.get_info();
        if !adapter.get_downlevel_capabilities().flags.contains(wgpu::DownlevelFlags::COMPUTE_SHADERS) {
            return Err(format!("{} ({:?}): no compute shaders", info.name, info.backend));
        }
        // OpenGL (Window Graphics ▸ OpenGL for drivers that crash otherwise, #243) translates the
        // kernels for tens of seconds before one fails validation: CPU compositing at once.
        if info.backend == wgpu::Backend::Gl {
            return Err(format!("{} (Gl): the GPU compositor needs DirectX 12, Vulkan, Metal or WebGPU", info.name));
        }
        // egui-wgpu asks for WebGL2's limits on GL (no storage buffers at all): building the
        // pipelines there would be a validation error, which release builds only log.
        let l = device.limits();
        let needs = [
            // `adv3d`'s post kernels bind 7 (its rasteriser's fragment stage 6).
            ("max_storage_buffers_per_shader_stage", l.max_storage_buffers_per_shader_stage, 7),
            ("max_storage_textures_per_shader_stage", l.max_storage_textures_per_shader_stage, 1),
            ("max_bind_groups", l.max_bind_groups, 2),
            ("max_compute_workgroup_size_x", l.max_compute_workgroup_size_x, 64),
            ("max_compute_workgroup_size_y", l.max_compute_workgroup_size_y, 16),
            ("max_compute_invocations_per_workgroup", l.max_compute_invocations_per_workgroup, 256),
        ];
        if let Some((limit, have, need)) = needs.iter().find(|(_, have, need)| have < need) {
            return Err(format!("{} ({:?}): device limit {limit} is {have}, needs {need}", info.name, info.backend));
        }
        for f in [FORMAT, wgpu::TextureFormat::Rgba8Unorm] {
            if !adapter.get_texture_format_features(f).allowed_usages.contains(wgpu::TextureUsages::STORAGE_BINDING) {
                return Err(format!("{} ({:?}): {f:?} storage textures unsupported", info.name, info.backend));
            }
        }
        let readbacks = crate::readback::Readbacks::new()?;
        let name = format!("{} ({:?})", info.name, info.backend);
        let adv3d_unsupported = crate::adv3d::raster_unsupported(adapter);
        if let Some(why) = &adv3d_unsupported {
            log::info!("gpu {name}: Advanced 3D renders on the CPU ({why})");
        }
        let src = [
            include_str!("shaders/common.wgsl"),
            include_str!("shaders/kernels.wgsl"),
            include_str!("shaders/classic3d.wgsl"),
            include_str!("shaders/fx_color.wgsl"),
            include_str!("shaders/fx_distort.wgsl"),
            include_str!("shaders/fx_generate.wgsl"),
            include_str!("shaders/fx_key.wgsl"),
            include_str!("shaders/fx_stylize.wgsl"),
            include_str!("shaders/fx_noise.wgsl"),
            include_str!("shaders/fx_tone.wgsl"),
            include_str!("shaders/fx_warp.wgsl"),
            include_str!("shaders/fx_extra.wgsl"),
            include_str!("shaders/fx_depth.wgsl"),
            include_str!("shaders/fx_lut.wgsl"),
            include_str!("shaders/fx_sim.wgsl"),
            include_str!("shaders/fx_particles.wgsl"),
            include_str!("shaders/fx_vr.wgsl"),
            include_str!("shaders/fx_light.wgsl"),
            include_str!("shaders/fx_transition.wgsl"),
            include_str!("shaders/fx_text.wgsl"),
            include_str!("shaders/fx_time.wgsl"),
            include_str!("shaders/fx_pixel2.wgsl"),
            include_str!("shaders/fx_gen2.wgsl"),
            include_str!("shaders/sky.wgsl"),
        ]
        .concat();
        let module = init_resource(&device, "kernels shader module", || {
            device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("effectcraft kernels"), source: wgpu::ShaderSource::Wgsl(src.into()) })
        })?;
        let bgl = init_resource(&device, "kernels bind-group layout", || {
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("effectcraft kernels"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
                        count: None,
                    },
                    tex_entry(1),
                    tex_entry(2),
                    storage_tex_entry(3, FORMAT),
                    wgpu::BindGroupLayoutEntry {
                        binding: 4,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Storage { read_only: true },
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                ],
            })
        })?;
        let layout = init_resource(&device, "kernels pipeline layout", || {
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("effectcraft kernels"),
                bind_group_layouts: &[Some(&bgl)],
                immediate_size: 0,
            })
        })?;
        let storage = |binding: u32| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Storage { read_only: true }, has_dynamic_offset: false, min_binding_size: None },
            count: None,
        };
        let bgl_ext = init_resource(&device, "extended kernels bind-group layout", || {
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("effectcraft kernels (ext)"),
                entries: &[storage(0), storage(1), storage(2), storage(3)],
            })
        })?;
        let layout_ext = init_resource(&device, "extended kernels pipeline layout", || {
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("effectcraft kernels (ext)"),
                bind_group_layouts: &[Some(&bgl), Some(&bgl_ext)],
                immediate_size: 0,
            })
        })?;
        let build = |e: &'static str, layout: &wgpu::PipelineLayout| {
            init_resource(&device, e, || {
                device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                    label: Some(e),
                    layout: Some(layout),
                    module: &module,
                    entry_point: Some(e),
                    compilation_options: Default::default(),
                    cache: None,
                })
            })
        };
        // The compositor's own kernels are required; an effect kernel that doesn't build is
        // left out, and its effects render on the CPU (see `Enc::take_missing`).
        let mut pipelines = ENTRIES
            .iter()
            .chain(crate::adv3d::SKY_KERNELS)
            .map(|e| (e, &layout))
            .chain(EXT_ENTRIES.iter().map(|e| (e, &layout_ext)))
            .map(|(e, layout)| Ok((*e, build(e, layout)?)))
            .collect::<Result<HashMap<_, _>, String>>()?;
        let mut unavailable = vec![];
        for e in EFFECT_ENTRIES
            .iter()
            .chain(crate::fx_color::KERNELS)
            .chain(crate::fx_distort::KERNELS)
            .chain(crate::fx_generate::KERNELS)
            .chain(crate::fx_key::KERNELS)
            .chain(crate::fx_stylize::KERNELS)
            .chain(crate::fx_noise::KERNELS)
            .chain(crate::fx_tone::KERNELS)
            .chain(crate::fx_warp::KERNELS)
            .chain(crate::fx_extra::KERNELS)
            .chain(crate::fx_depth::KERNELS)
            .chain(crate::fx_lut::KERNELS)
            .chain(crate::fx_sim::KERNELS)
            .chain(crate::fx_particles::KERNELS)
            .chain(crate::fx_vr::KERNELS)
            .chain(crate::fx_light::KERNELS)
            .chain(crate::fx_transition::KERNELS)
            .chain(crate::fx_text::KERNELS)
            .chain(crate::fx_time::KERNELS)
            .chain(crate::fx_pixel2::KERNELS)
            .chain(crate::fx_gen2::KERNELS)
        {
            match build(e, &layout) {
                Ok(p) => {
                    pipelines.insert(*e, p);
                }
                Err(why) => {
                    log::warn!("gpu {name}: kernel {e} unavailable, its effects render on the CPU: {why}");
                    unavailable.push((*e, why));
                }
            }
        }
        let dmodule = init_resource(&device, "display shader module", || {
            device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("effectcraft display"),
                source: wgpu::ShaderSource::Wgsl(include_str!("shaders/display.wgsl").into()),
            })
        })?;
        let display_bgl = init_resource(&device, "display bind-group layout", || {
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("effectcraft display"),
                entries: &[tex_entry(1), storage_tex_entry(3, wgpu::TextureFormat::Rgba8Unorm)],
            })
        })?;
        let dlayout = init_resource(&device, "display pipeline layout", || {
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("effectcraft display"),
                bind_group_layouts: &[Some(&display_bgl)],
                immediate_size: 0,
            })
        })?;
        let display_pipeline = |entry: &'static str| {
            init_resource(&device, "display compute pipeline", || {
                device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                    label: Some(entry),
                    layout: Some(&dlayout),
                    module: &dmodule,
                    entry_point: Some(entry),
                    compilation_options: Default::default(),
                    cache: None,
                })
            })
        };
        let display = display_pipeline("display")?;
        let display_mip = display_pipeline("mip")?;
        let dummy = init_resource(&device, "dummy texture", || {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some("dummy"),
                size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: FORMAT,
                usage: wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            })
        })?;
        let dummy_buf =
            init_resource(&device, "dummy storage buffer", || buffer_init(&device, &queue, Some("dummy"), &[0u8; 16], wgpu::BufferUsages::STORAGE))?;
        let dummy_tex = init_resource(&device, "dummy texture view", || dummy.create_view(&Default::default()))?;
        let generation = effectcraft_render::allocation::GpuGeneration::default();
        let allocation = effectcraft_render::allocation::GpuAllocation::rgba_f32(1, 1, generation.clone());
        let dummy = GpuImage { allocation, texture: dummy, width: 1, height: 1, view: dummy_tex, _lease: None, levels: None };
        let align = u64::from(device.limits().min_uniform_buffer_offset_alignment.max(1));
        let uniform_stride = PARAMS_SIZE.div_ceil(align).saturating_mul(align);
        let max_dim = device.limits().max_texture_dimension_2d;
        let integrated = info.device_type == wgpu::DeviceType::IntegratedGpu;
        let (upload_budget, pool_budget, auxiliary_budget) = cache_budgets(UPLOAD_BUDGET, integrated);
        let yuv = match crate::yuv::Pipelines::new(adapter, &device) {
            Ok(p) => Some(p),
            Err(reason) => {
                unavailable.push(("yuv", reason));
                None
            }
        };
        let static_pipeline_count = std::sync::atomic::AtomicU64::new(pipelines.len() as u64 + 2 + if yuv.is_some() { 2 } else { 0 });
        Ok(GpuContext {
            generation,
            device,
            queue,
            name,
            max_dim,
            bgl,
            bgl_ext,
            pipelines,
            fusion: Default::default(),
            yuv,
            unavailable,
            display_bgl,
            display,
            display_mip,
            dummy,
            dummy_buf,
            uniform_stride,
            uniforms: Default::default(),
            uploads: Arc::new(Mutex::new(Uploads { budget: Some(upload_budget), ..Default::default() })),
            pool: Arc::new(Mutex::new(Pool { budget: Some(pool_budget), ..Default::default() })),
            integrated,
            auxiliary_budget: std::sync::atomic::AtomicU64::new(auxiliary_budget),
            staging: Default::default(),
            transfers: Default::default(),
            zeros: Default::default(),
            adv3d: std::sync::OnceLock::new(),
            static_pipeline_count,
            adv3d_raster: adv3d_unsupported.is_none(),
            particles: Default::default(),
            particle_states: Default::default(),
            deferred: None,
            readbacks,
        })
    }

    /// Whether this context has observed a readback timeout or device failure.
    /// A successful check is not a guarantee against faults after it returns.
    /// Shared-device hosts must forward their device failures through the notifier.
    pub fn check_health(&self) -> Result<(), String> {
        let result = self.readbacks.check();
        if result.is_err() {
            self.generation.retire();
            clear_retained(&self.uploads);
        }
        result
    }

    /// A notification hook for the owner of a shared device's error/loss handlers.
    /// Invoke it with diagnostic context when the host observes a device failure.
    /// It retires this context and settles pending readbacks without replacing any
    /// host handler or keeping this context/device alive. It is harmless after drop.
    ///
    /// Shared-device construction does not automatically subscribe to device faults:
    /// without host forwarding, readbacks rely on poll errors and their deadlines.
    pub fn device_failure_notifier(&self) -> impl Fn(String) + Send + Sync + 'static {
        let notify = crate::readback::failure_notifier(&self.readbacks);
        let uploads = Arc::downgrade(&self.uploads);
        let generation = self.generation.clone();
        move |message| {
            generation.retire();
            notify(message);
            if let Some(uploads) = uploads.upgrade() {
                clear_retained(&uploads);
            }
        }
    }

    /// All device polling routes through this failure bridge.
    pub(crate) fn poll_readbacks(&self, kind: wgpu::PollType) -> Result<wgpu::PollStatus, String> {
        self.readbacks.check()?;
        self.device.poll(kind).map_err(|e| {
            let error = format!("GPU readback poll: {e}");
            self.readbacks.retire(error.clone());
            log::error!("{error}");
            error
        })
    }

    /// Start a bounded buffer mapping. Retired contexts reject new requests immediately.
    pub(crate) fn map_readback(
        &self,
        buffer: &wgpu::Buffer,
        row: usize,
        stride: usize,
        height: usize,
        done: impl FnOnce(Result<Vec<u8>, String>) + wgpu::WasmNotSend + 'static,
    ) {
        let ticket = match self.readbacks.start(done) {
            Ok(t) => t,
            Err(e) => {
                log::error!("gpu: {e}");
                return;
            }
        };
        if !ticket.active() {
            return;
        }
        let b = buffer.clone();
        buffer.map_async(wgpu::MapMode::Read, .., move |r| {
            if !ticket.active() {
                b.unmap();
                return;
            }
            let result = r.map_err(|e| format!("GPU buffer mapping: {e}")).and_then(|_| {
                let view = b.get_mapped_range(..).map_err(|e| format!("GPU mapped range: {e}"))?;
                crate::readback::packed(&view, row, stride, height)
            });
            b.unmap();
            ticket.complete(result);
        });
    }

    /// A bounded native readback: polling and receiving share one deadline.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn read_buffer(&self, buffer: &wgpu::Buffer, row: usize, stride: usize, height: usize) -> Result<Vec<u8>, String> {
        self.read_mapped(buffer, None, |view| crate::readback::packed(view, row, stride, height))
    }

    #[cfg(target_arch = "wasm32")]
    pub(crate) fn read_buffer(&self, _buffer: &wgpu::Buffer, _row: usize, _stride: usize, _height: usize) -> Result<Vec<u8>, String> {
        Err("synchronous GPU readback is unavailable in the browser".into())
    }

    /// Map `buffer`, wait for it (bounded: polling and receiving share one deadline) and `read`
    /// the mapped bytes in place. With `submission` (the one that wrote the buffer) only that
    /// submission is waited for, not the GPU work other render workers submitted since.
    #[cfg(not(target_arch = "wasm32"))]
    fn read_mapped<T>(
        &self,
        buffer: &wgpu::Buffer,
        submission: Option<wgpu::SubmissionIndex>,
        read: impl FnOnce(&[u8]) -> Result<T, String>,
    ) -> Result<T, String> {
        let deadline = std::time::Instant::now() + crate::readback::TIMEOUT;
        let (tx, rx) = std::sync::mpsc::channel();
        let unmap = buffer.clone();
        let ticket = self.readbacks.start(move |r| {
            // Nobody waits any more (timed out): don't leave the buffer mapped.
            if let Err(std::sync::mpsc::SendError(Ok(_))) = tx.send(r) {
                unmap.unmap();
            }
        })?;
        if ticket.active() {
            let b = buffer.clone();
            buffer.map_async(wgpu::MapMode::Read, .., move |r| {
                if !ticket.active() {
                    b.unmap();
                    return;
                }
                match r {
                    // The waiter reads the mapped range and unmaps.
                    Ok(()) => ticket.complete(Ok(Vec::new())),
                    Err(e) => {
                        b.unmap();
                        ticket.complete(Err(format!("GPU buffer mapping: {e}")));
                    }
                }
            });
        }
        loop {
            self.readbacks.check()?;
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                let error = "GPU readback receive timed out; context retired".to_string();
                self.readbacks.retire(error.clone());
                return Err(error);
            }
            // Block until the GPU is done (bounded by the deadline: a timeout retires the
            // context) rather than polling and sleeping, which adds latency to every readback.
            self.poll_readbacks(wgpu::PollType::Wait { submission_index: submission.clone(), timeout: Some(remaining) })?;
            // The callback may be running on another thread that polled the same device.
            match rx.recv_timeout(remaining.min(std::time::Duration::from_millis(1))) {
                Ok(result) => {
                    result?;
                    break;
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(e) => return Err(format!("GPU readback completion channel: {e}")),
            }
        }
        let result = buffer.get_mapped_range(..).map_err(|e| format!("GPU mapped range: {e}")).and_then(|view| read(&view));
        buffer.unmap();
        result
    }

    /// Switch to deferred readbacks (see [`crate::deferred`]): readbacks are never waited for.
    pub fn set_deferred(&mut self, on: bool) {
        self.readbacks.cancel_pending("GPU readback mode changed".into());
        if let Some(d) = self.deferred.take() {
            d.cancel();
        }
        self.deferred = on.then(Default::default);
    }

    pub fn deferred(&self) -> Option<&Arc<crate::deferred::Deferred>> {
        self.deferred.as_ref()
    }

    /// A device of its own on the best adapter, without blocking (a browser worker: WebGPU
    /// only; natively any backend). `Err` says why there is none.
    pub async fn request() -> Result<GpuContext, String> {
        #[allow(unused_mut)]
        let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
        #[cfg(not(target_arch = "wasm32"))]
        {
            // Match eframe's backend override so CLI and headless acceptance tests
            // exercise the backend requested for the native viewer.
            desc.backends = wgpu::Backends::from_env().unwrap_or(desc.backends);
        }
        #[cfg(target_arch = "wasm32")]
        {
            desc.backends = wgpu::Backends::BROWSER_WEBGPU;
        }
        let instance = wgpu::Instance::new(desc);
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions { power_preference: wgpu::PowerPreference::HighPerformance, ..Default::default() })
            .await
            .map_err(|e| format!("no adapter: {e}"))?;
        Self::request_on_adapter(&adapter).await
    }

    /// Request a separate device on an existing adapter (the desktop window's adapter).
    pub async fn request_on_adapter(adapter: &wgpu::Adapter) -> Result<GpuContext, String> {
        let limits = adapter.limits();
        let required_limits = wgpu::Limits {
            max_texture_dimension_2d: limits.max_texture_dimension_2d.min(16384),
            max_buffer_size: limits.max_buffer_size,
            ..wgpu::Limits::default()
        };
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("effectcraft gpu"),
                required_limits,
                required_features: profile_features(adapter.features()),
                ..Default::default()
            })
            .await
            .map_err(|e| format!("no device: {e}"))?;
        let context = GpuContext::new(adapter, device, queue)?;
        // This path owns the device; install handlers only after construction succeeds.
        crate::readback::install(&context.device, &context.readbacks);
        Ok(context)
    }

    /// A device of its own on the best adapter (CLI, tests, benchmarks). `None` when no
    /// adapter qualifies.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn headless() -> Option<GpuContext> {
        pollster::block_on(GpuContext::request()).map_err(|e| log::info!("gpu: {e}")).ok()
    }

    /// Readbacks (GPU → CPU) are possible: waited for natively, or deferred (a browser
    /// worker, [`crate::deferred`]). Impossible on the browser's main thread.
    /// Effect kernels the device couldn't build, with why: their effects render on the CPU.
    pub fn unavailable_kernels(&self) -> &[(&'static str, String)] {
        &self.unavailable
    }

    /// Leave kernel `entry` out, as if it had failed to build (tests of the CPU fallback).
    #[cfg(test)]
    pub(crate) fn drop_kernel(&mut self, entry: &'static str) {
        self.pipelines.remove(entry);
        self.unavailable.push((entry, "dropped by a test".into()));
    }

    pub fn can_readback(&self) -> bool {
        self.readbacks.check().is_ok() && (self.can_wait() || self.deferred.is_some())
    }

    /// Readbacks can be waited for (natively, unless deferred). Steps that read back without a
    /// key ([`crate::deferred`]) need this.
    pub fn can_wait(&self) -> bool {
        self.readbacks.check().is_ok() && cfg!(not(target_arch = "wasm32")) && self.deferred.is_none()
    }

    pub(crate) fn fits(&self, w: u32, h: u32) -> bool {
        w > 0 && h > 0 && w <= self.max_dim && h <= self.max_dim
    }

    /// A new (zeroed) texture of its own (not pooled).
    pub(crate) fn image(&self, w: u32, h: u32) -> GpuImage {
        GpuImage::in_generation(self.texture(w, h), w, h, self.generation.clone())
    }

    fn texture(&self, w: u32, h: u32) -> wgpu::Texture {
        self.device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d { width: w.max(1), height: h.max(1), depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: FORMAT,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        })
    }

    /// A pooled working texture: (image, reused). A reused texture holds old pixels; a new
    /// one is zeroed.
    fn pooled_for_encoder(&self, w: u32, h: u32, encoder: Option<u64>) -> (GpuImage, bool) {
        #[cfg(feature = "profile")]
        let encoder = {
            static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
            if *ENABLED.get_or_init(|| std::env::var("EFFECTCRAFT_ENCODER_REUSE").as_deref() != Ok("0")) { encoder } else { None }
        };

        let (w, h) = (w.max(1), h.max(1));
        let reused = self.pool.lock().ok().and_then(|mut p| p.take_for_encoder(w, h, encoder));
        let was_reused = reused.is_some();
        let texture = reused.unwrap_or_else(|| self.texture(w, h));
        let lease = Arc::new(Lease { texture: texture.clone(), pool: Arc::downgrade(&self.pool) });
        let view = texture.create_view(&Default::default());
        let allocation = effectcraft_render::allocation::GpuAllocation::rgba_f32(w, h, self.generation.clone());
        (GpuImage { allocation, texture, width: w, height: h, view, _lease: Some(lease), levels: None }, was_reused)
    }

    /// Uploads and readbacks so far (images; uploads of cached layers count once).
    pub fn transfer_stats(&self) -> TransferStats {
        use std::sync::atomic::Ordering::Relaxed;
        let t = &self.transfers;
        TransferStats {
            uploads: t.ups.load(Relaxed),
            upload_bytes: t.up_bytes.load(Relaxed),
            readbacks: t.downs.load(Relaxed),
            readback_bytes: t.down_bytes.load(Relaxed),
            yuv_conversions: t.yuv_conversions.load(Relaxed),
            yuv_bytes: t.yuv_bytes.load(Relaxed),
        }
    }

    /// Texture pool counters: (working textures reused, newly allocated).
    pub fn pool_stats(&self) -> (u64, u64) {
        self.pool.lock().map(|p| (p.hits, p.misses)).unwrap_or_default()
    }

    /// Upload a CPU image (`None` when it does not fit the device).
    pub fn upload_image(&self, img: &Image) -> Option<GpuImage> {
        let _profile = effectcraft_render::profile::span("upload");
        if !self.fits(img.width, img.height) {
            return None;
        }
        self.transfers.count(true, img.data.len() * 16);
        let g = self.image(img.width, img.height);
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo { texture: &g.texture, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            bytemuck::cast_slice(&img.data),
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(img.width * 16), rows_per_image: Some(img.height) },
            wgpu::Extent3d { width: img.width, height: img.height, depth_or_array_layers: 1 },
        );
        Some(g)
    }

    /// Upload a (layer cache) buffer, reusing the texture while the same `Arc` is alive: static
    /// layers upload once, not every frame.
    fn upload_epoch(&self) -> Weak<()> {
        self.uploads.lock().map(|u| Arc::downgrade(&u.epoch)).unwrap_or_default()
    }
    fn prune_uploads(&self) {
        loop {
            let stale = self.uploads.lock().ok().and_then(|mut u| u.stale());
            let Some(stale) = stale else { break };
            stale.release();
        }
    }
    fn retained_cap(&self) -> usize {
        self.uploads.lock().map(|u| u.budget.unwrap_or(UPLOAD_BUDGET)).unwrap_or(0)
    }
    fn upload_buf_owned(&self, buf: &Arc<Buf>, owner: Option<&GpuOwner>) -> Option<GpuImage> {
        let epoch = self.upload_epoch();
        self.prune_uploads();
        if let Some(owner) = owner {
            let key = (owner.identity(), Arc::as_ptr(buf) as usize);
            let entry = self.uploads.lock().ok().and_then(|u| u.map.get(&key).cloned());
            if let Some(entry) = entry
                && entry.buf.ptr_eq(&Arc::downgrade(buf))
                && let Some(img) = entry.slot.get(&self.generation, GpuStage::CpuMirror)
            {
                return Some(img);
            }
        }
        let img = self.upload_image(&buf.img)?;
        if let Some(owner) = owner {
            self.register(buf, img.clone(), owner, &epoch);
        }
        Some(img)
    }
    fn register(&self, buf: &Arc<Buf>, img: GpuImage, owner: &GpuOwner, epoch: &Weak<()>) {
        let Some(allocation) = img.allocation.clone() else { return };
        if !allocation.generation().same(&self.generation) {
            return;
        }
        let Ok(pending) = owner.prepare(Arc::new(img), allocation, GpuStage::CpuMirror, self.retained_cap()) else { return };
        let Ok(slot) = pending.publish() else { return };
        let entry = SlotRef { owner: owner.clone(), slot, epoch: epoch.clone() };
        let key = (owner.identity(), Arc::as_ptr(buf) as usize);
        let old = {
            let mut u = match self.uploads.lock() {
                Ok(u) => u,
                Err(poison) => {
                    drop(poison);
                    entry.release();
                    return;
                }
            };
            if !self.generation.valid()
                || !self.generation.within_cap()
                || epoch.strong_count() == 0
                || u.map.len() >= effectcraft_render::retained::MAX_RECORDS
                || u.map.try_reserve(1).is_err()
            {
                drop(u);
                entry.release();
                return;
            }
            u.map.insert(key, Upload { buf: Arc::downgrade(buf), slot: entry }).map(|e| e.slot)
        };
        if let Some(old) = old {
            old.release();
        }
    }
    fn keep_owned(&self, img: GpuImage, owner: Option<&GpuOwner>, epoch: &Weak<()>) -> Option<u64> {
        let owner = owner?;
        self.prune_uploads();
        if self.uploads.lock().ok()?.keep_off {
            return None;
        }
        let allocation = img.allocation.clone()?;
        if !allocation.generation().same(&self.generation) {
            return None;
        }
        let slot = owner.prepare(Arc::new(img), allocation, GpuStage::Processed, self.retained_cap()).ok()?.publish().ok()?;
        let entry = SlotRef { owner: owner.clone(), slot, epoch: epoch.clone() };
        let result = {
            let mut u = match self.uploads.lock() {
                Ok(u) => u,
                Err(poison) => {
                    drop(poison);
                    entry.release();
                    return None;
                }
            };
            if !self.generation.valid()
                || !self.generation.within_cap()
                || epoch.strong_count() == 0
                || u.keep_off
                || u.kept.len() >= MAX_KEPT
                || u.kept.try_reserve(1).is_err()
            {
                drop(u);
                entry.release();
                return None;
            }
            let Some(ticket) = u.next_ticket.checked_add(1) else {
                drop(u);
                entry.release();
                return None;
            };
            u.next_ticket = ticket;
            u.kept.push((ticket, entry));
            ticket
        };
        Some(result)
    }
    pub(crate) fn adopt(&self, ticket: u64, buf: &Arc<Buf>) {
        let Some(entry) = self.take_kept(ticket) else { return };
        if let Some(img) = entry.get::<GpuImage>(&self.generation, GpuStage::Processed)
            && (img.width, img.height) == (buf.img.width, buf.img.height)
            && !buf.img.is_empty()
        {
            self.register(buf, img, &entry.owner, &entry.epoch);
        }
        entry.release();
    }
    pub(crate) fn release(&self, ticket: u64) {
        if let Some(entry) = self.take_kept(ticket) {
            entry.release();
        }
    }
    fn take_kept(&self, ticket: u64) -> Option<SlotRef> {
        let mut u = self.uploads.lock().ok()?;
        let i = u.kept.iter().position(|(t, _)| *t == ticket)?;
        Some(u.kept.remove(i).1)
    }
    #[cfg(test)]
    pub(crate) fn kept_count(&self) -> usize {
        self.prune_uploads();
        self.uploads.lock().map(|u| u.kept.len()).unwrap_or_default()
    }
    #[cfg(test)]
    pub(crate) fn set_keep_effects(&self, on: bool) {
        let old = if let Ok(mut u) = self.uploads.lock() {
            u.keep_off = !on;
            std::mem::take(&mut u.kept)
        } else {
            Vec::new()
        };
        for (_, entry) in old {
            entry.release();
        }
    }

    /// Size the GPU-side caches after the layer cache's memory budget (Memory & CPU settings,
    /// after RAM Reserved for Other Applications): uploaded layer textures mirror cached layer
    /// buffers, so they get at most the same budget (capped at [`UPLOAD_BUDGET`]), and free
    /// working textures half of it (capped at [`POOL_BUDGET`]). On unified memory (Apple
    /// silicon, Adreno) CPU/GPU caches share RAM, so retained uploads and working textures
    /// are further capped at 256/128 MiB. Auxiliary staging and zero-image caches each get
    /// 1/16 of the layer budget, capped at 32 MiB integrated or 64 MiB discrete.
    pub fn set_cache_budget(&self, layer_cache: usize) {
        let (upload, pool, auxiliary) = cache_budgets(layer_cache, self.integrated);
        self.auxiliary_budget.store(auxiliary, std::sync::atomic::Ordering::Relaxed);
        if let Ok(mut u) = self.uploads.lock() {
            u.budget = Some(upload);
            self.generation.set_cap(upload);
        }
        let owners = self
            .uploads
            .lock()
            .map(|u| {
                u.map
                    .values()
                    .map(|e| e.slot.owner.clone())
                    .chain(u.native.values().map(|e| e.slot.owner.clone()))
                    .chain(u.kept.iter().map(|(_, e)| e.owner.clone()))
                    .chain(u.resident.iter().map(|e| e.owner.clone()))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        for owner in owners {
            owner.enforce_device_cap(&self.generation, upload);
        }
        self.prune_uploads();
        if let Ok(mut p) = self.pool.lock() {
            p.set_budget(pool);
        }
        if let Ok(mut s) = self.staging.lock() {
            let retained = s.iter().map(|(size, buffers)| size.saturating_mul(buffers.len() as u64)).sum::<u64>();
            if retained > auxiliary {
                s.clear();
            }
        }
        if let Ok(mut z) = self.zeros.lock() {
            let retained = z.values().map(|img| u64::from(img.width).saturating_mul(u64::from(img.height)).saturating_mul(16)).sum::<u64>();
            if retained > auxiliary {
                z.clear();
            }
        }
    }

    pub(crate) fn native_get(
        &self,
        owner: &effectcraft_render::footage::SharedFrame,
        key: u64,
        token: &effectcraft_render::cache::ResidentToken,
        metadata: &[u64],
        cache: Option<&GpuOwner>,
    ) -> Option<NativeImage> {
        let cache = cache?;
        let _profile = effectcraft_render::profile::span("native_cache_lookup");
        if !token.valid() || token.identity() != cache.identity() || self.check_health().is_err() {
            return None;
        }
        self.prune_uploads();
        let k = (Arc::as_ptr(owner) as *const () as usize, token.identity(), key);
        let entry = self.uploads.lock().ok()?.native.get(&k).cloned()?;
        if entry.metadata != metadata || !entry.owner.ptr_eq(&Arc::downgrade(owner)) {
            return None;
        }
        entry.slot.get(&self.generation, GpuStage::Blend)
    }
    fn native_keep(&self, entry: PendingNative) {
        if !entry.token.valid() || entry.epoch.strong_count() == 0 || self.check_health().is_err() {
            return;
        }
        let Ok(slot) = entry.pending.publish() else { return };
        let retained = SlotRef { owner: entry.cache, slot, epoch: entry.epoch.clone() };
        let k = (Arc::as_ptr(&entry.owner) as *const () as usize, entry.token.identity(), entry.key);
        let old = {
            let mut u = match self.uploads.lock() {
                Ok(u) => u,
                Err(poison) => {
                    drop(poison);
                    retained.release();
                    return;
                }
            };
            if !self.generation.valid()
                || !self.generation.within_cap()
                || entry.epoch.strong_count() == 0
                || u.native.len() >= 1024
                || u.native.try_reserve(1).is_err()
            {
                drop(u);
                retained.release();
                return;
            }
            u.native
                .insert(k, NativeUpload { owner: Arc::downgrade(&entry.owner), token: entry.token, metadata: entry.metadata, slot: retained })
                .map(|e| e.slot)
        };
        if let Some(old) = old {
            old.release();
        }
    }
    /// Live indexed uploads. The authoritative physical accounting is the layer owner.
    pub fn upload_stats(&self) -> (usize, usize) {
        self.prune_uploads();
        let entries = self
            .uploads
            .lock()
            .map(|u| u.map.values().map(|e| e.slot.clone()).chain(u.native.values().map(|e| e.slot.clone())).collect::<Vec<_>>())
            .unwrap_or_default();
        let mut identities = BTreeSet::new();
        let mut bytes = 0usize;
        let mut count = 0usize;
        for entry in entries {
            let slot = entry.slot.upgrade();
            if let Some(slot) = slot {
                count += 1;
                if identities.insert(Arc::as_ptr(slot.allocation()) as usize) {
                    bytes = bytes.saturating_add(slot.allocation().bytes());
                }
            }
        }
        (count, bytes)
    }
    /// Healthy clear invalidates pending native publication without retiring the device.
    pub fn clear_uploads(&self) {
        clear_retained(&self.uploads);
        if let Ok(mut p) = self.pool.lock() {
            p.clear();
        }
        let old = self.staging.lock().ok().map(|mut s| std::mem::take(&mut *s));
        drop(old);
        let old = self.zeros.lock().ok().map(|mut z| std::mem::take(&mut *z));
        drop(old);
    }
}

impl Drop for GpuContext {
    fn drop(&mut self) {
        self.generation.retire();
        clear_retained(&self.uploads);
    }
}

fn profile_features(available: wgpu::Features) -> wgpu::Features {
    if cfg!(feature = "profile") {
        available & (wgpu::Features::TIMESTAMP_QUERY | wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS)
    } else {
        wgpu::Features::empty()
    }
}

/// Records GPU work for one render (a command encoder submitted at readback or the end).
struct PendingNative {
    owner: effectcraft_render::footage::SharedFrame,
    key: u64,
    token: effectcraft_render::cache::ResidentToken,
    metadata: Vec<u64>,
    cache: GpuOwner,
    pending: GpuPending,
    epoch: Weak<()>,
}

struct LazyReadback(Weak<GpuContext>);
impl effectcraft_render::cache::ResidentReadback for LazyReadback {
    fn readback(&self, slot: &GpuSlot) -> Option<Buf> {
        let ctx = self.0.upgrade()?;
        if slot.stage() != GpuStage::Processed || !slot.allocation().generation().same(&ctx.generation) {
            return None;
        }
        let image = slot.payload::<NativeImage>()?;
        crate::Gpu::within_context(&ctx, || {
            let mut enc = Enc::new(&ctx);
            Some(Buf { img: enc.download(&image.img)?, offset: image.offset, scale: image.scale })
        })
        .ok()
        .flatten()
    }
}

pub(crate) struct Enc<'g> {
    epoch: Weak<()>,
    pub(crate) owner: Option<GpuOwner>,
    native_pending: Vec<PendingNative>,
    resident_pending: Vec<(GpuPending, effectcraft_render::cache::FlightGuard)>,
    readback: Option<Arc<dyn effectcraft_render::cache::ResidentReadback>>,
    #[cfg(feature = "profile")]
    pub(crate) timestamps: Option<crate::timestamps::Batch>,
    pub g: &'g GpuContext,
    /// The open command encoder and its id in the texture pool.
    enc: Option<(wgpu::CommandEncoder, u64)>,
    pending: usize,
    /// The latest submission (readbacks wait for it, not for every worker's GPU work).
    submitted: Option<wgpu::SubmissionIndex>,
    /// This encoder's uniform ring (see [`Enc::dispatch_ext`]).
    ring: Option<wgpu::Buffer>,
    /// A dispatch named a kernel the device doesn't have (see [`Enc::take_missing`]).
    missing: bool,
}

impl Drop for Enc<'_> {
    fn drop(&mut self) {
        // Unsubmitted work is discarded, so nothing recorded will read pooled textures.
        if let Some((_, id)) = self.enc.take()
            && let Ok(mut p) = self.g.pool.lock()
        {
            p.close(id);
        }
        if let Some(ring) = self.ring.take()
            && let Ok(mut u) = self.g.uniforms.lock()
            && u.len() < RINGS_KEPT
        {
            u.push(ring);
        }
    }
}

impl<'g> Enc<'g> {
    pub fn new(g: &'g GpuContext) -> Enc<'g> {
        Self::with_owner(g, None)
    }
    pub fn with_resident(g: &'g Arc<GpuContext>, owner: Option<GpuOwner>) -> Enc<'g> {
        let mut enc = Self::with_owner(g, owner);
        enc.readback = Some(Arc::new(LazyReadback(Arc::downgrade(g))));
        enc
    }
    pub(crate) fn can_retain_resident(&self) -> bool {
        self.readback.is_some() && self.owner.as_ref().is_some_and(GpuOwner::valid) && self.g.deferred.is_none()
    }
    pub(crate) fn resident(&self, key: u64, stage: GpuStage) -> Option<NativeImage> {
        self.owner.as_ref()?.resident_in(key, stage, Some(&self.g.generation))?.payload::<NativeImage>().cloned()
    }
    pub(crate) fn keep_resident(
        &mut self,
        key: u64,
        image: NativeImage,
        flight: effectcraft_render::cache::FlightGuard,
        intent: Option<effectcraft_render::disk_cache::LayerWriteIntent>,
    ) {
        let Some(owner) = self.owner.as_ref() else { return };
        if self.g.deferred.is_some() || self.resident_pending.len() >= 256 || self.epoch.strong_count() == 0 {
            return;
        }
        let Some(allocation) = image.img.allocation.clone() else { return };
        let Ok(pending) = owner.prepare(Arc::new(image), allocation, GpuStage::Processed, self.g.retained_cap()) else { return };
        if self.resident_pending.try_reserve(1).is_ok() {
            self.resident_pending.push((pending.with_content(key, self.readback.clone()).with_persistence(intent), flight));
        }
    }
    pub fn with_owner(g: &'g GpuContext, owner: Option<GpuOwner>) -> Enc<'g> {
        Enc {
            epoch: g.upload_epoch(),
            owner,
            native_pending: Vec::new(),
            resident_pending: Vec::new(),
            readback: None,
            g,
            enc: None,
            pending: 0,
            submitted: None,
            ring: None,
            missing: false,
            #[cfg(feature = "profile")]
            timestamps: None,
        }
    }

    /// Whether a dispatch since the last call named a kernel the device doesn't have (one
    /// that failed to build): its output is wrong, and the effect must render on the CPU.
    pub(crate) fn take_missing(&mut self) -> bool {
        std::mem::take(&mut self.missing)
    }

    pub(crate) fn encoder(&mut self) -> &mut wgpu::CommandEncoder {
        let g = self.g;
        &mut self
            .enc
            .get_or_insert_with(|| {
                let id = g.pool.lock().map(|mut p| p.open()).unwrap_or(0);
                (g.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("effectcraft") }), id)
            })
            .0
    }

    /// A new zeroed image (pooled; a reused texture is cleared by a kernel).
    pub fn image(&mut self, w: u32, h: u32) -> GpuImage {
        self.encoder();
        let owner = self.enc.as_ref().map(|(_, id)| *id);
        let (img, reused) = self.g.pooled_for_encoder(w, h, owner);
        if reused {
            self.fill(&img, 0.0);
        }
        img
    }

    /// A new image for a kernel that writes every pixel of it (pooled; contents undefined).
    /// Under test it starts out as NaNs, so a kernel that skips pixels fails the CPU oracle.
    pub fn scratch(&mut self, w: u32, h: u32) -> GpuImage {
        self.encoder();
        let owner = self.enc.as_ref().map(|(_, id)| *id);
        let (img, _) = self.g.pooled_for_encoder(w, h, owner);
        if cfg!(test) {
            self.fill(&img, f32::NAN);
        }
        img
    }

    /// A transparent image to read from (shared per size; never written).
    pub fn zeros(&self, w: u32, h: u32) -> GpuImage {
        let (w, h) = (w.max(1), h.max(1));
        let Ok(mut z) = self.g.zeros.lock() else { return self.g.image(w, h) };
        if let Some(img) = z.get(&(w, h)) {
            return img.clone();
        }
        let bytes = u64::from(w).saturating_mul(u64::from(h)).saturating_mul(16);
        let budget = self.g.auxiliary_budget.load(std::sync::atomic::Ordering::Relaxed);
        // Oversized images are valid working resources, but must not be retained forever.
        if bytes > budget {
            return self.g.image(w, h);
        }
        let retained = z.values().map(|img| u64::from(img.width).saturating_mul(u64::from(img.height)).saturating_mul(16)).sum::<u64>();
        if retained.saturating_add(bytes) > budget {
            z.clear();
        }
        let img = self.g.image(w, h);
        z.insert((w, h), img.clone());
        img
    }

    /// Set every pixel of `img` to `v`.
    fn fill(&mut self, img: &GpuImage, v: f32) {
        let mut p = Params::default();
        p.f[0] = [v; 4];
        let dummy = self.g.dummy.clone();
        self.pixels("fill", &p, &dummy, None, img, None);
    }

    /// Run kernel `entry` writing `out` (dispatched over `groups` workgroups).
    pub fn dispatch(
        &mut self,
        entry: &str,
        p: &Params,
        src: &GpuImage,
        aux: Option<&GpuImage>,
        out: &GpuImage,
        data: Option<&wgpu::Buffer>,
        groups: (u32, u32),
    ) {
        self.dispatch_ext(entry, p, src, aux, out, data, groups, None);
    }

    /// [`Enc::dispatch`] for the kernels in `EXT_ENTRIES`, with their group 1 storage buffers.
    #[allow(clippy::too_many_arguments)]
    pub fn dispatch_ext(
        &mut self,
        entry: &str,
        p: &Params,
        src: &GpuImage,
        aux: Option<&GpuImage>,
        out: &GpuImage,
        data: Option<&wgpu::Buffer>,
        groups: (u32, u32),
        ext: Option<[&wgpu::Buffer; 4]>,
    ) {
        let g = self.g;
        let Some(pipe) = g.pipelines.get(entry) else {
            if !g.unavailable.iter().any(|(e, _)| *e == entry) {
                log::error!("gpu: no kernel {entry}");
            }
            self.missing = true;
            return;
        };
        self.dispatch_pipeline(entry, pipe, p, src, aux, out, data, groups, ext);
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn dispatch_pipeline(
        &mut self,
        entry: &str,
        pipe: &wgpu::ComputePipeline,
        p: &Params,
        src: &GpuImage,
        aux: Option<&GpuImage>,
        out: &GpuImage,
        data: Option<&wgpu::Buffer>,
        groups: (u32, u32),
        ext: Option<[&wgpu::Buffer; 4]>,
    ) {
        let g = self.g;
        // Each dispatch until the next submit gets its own block of the encoder's uniform ring.
        // `write_buffer` lands before the next submission's commands and after earlier
        // submissions', so a ring is reused (after a submit, or by a later encoder) safely.
        if self.pending >= RING_SLOTS {
            self.submit();
        }
        let offset = (self.pending as u64).saturating_mul(g.uniform_stride);
        let ring = self.ring.get_or_insert_with(|| {
            g.uniforms.lock().ok().and_then(|mut u| u.pop()).unwrap_or_else(|| {
                g.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("params"),
                    size: g.uniform_stride.saturating_mul(RING_SLOTS as u64),
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                })
            })
        });
        g.queue.write_buffer(ring, offset, &p.bytes());
        let bg = g.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &g.bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding { buffer: ring, offset, size: wgpu::BufferSize::new(PARAMS_SIZE) }),
                },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&src.view) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(aux.map_or(&g.dummy.view, |a| &a.view)) },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(&out.view) },
                wgpu::BindGroupEntry { binding: 4, resource: data.unwrap_or(&g.dummy_buf).as_entire_binding() },
            ],
        });
        let bg_ext = ext.map(|b| {
            g.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: None,
                layout: &g.bgl_ext,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: b[0].as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 1, resource: b[1].as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 2, resource: b[2].as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 3, resource: b[3].as_entire_binding() },
                ],
            })
        });
        #[cfg(feature = "profile")]
        let mut batch = self.timestamps.take().or_else(|| crate::timestamps::Batch::new(g));
        #[cfg(feature = "profile")]
        let timestamp_writes = batch.as_mut().and_then(|b| b.mark(entry));
        #[cfg(not(feature = "profile"))]
        let timestamp_writes = None;
        let enc = self.encoder();
        {
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some(entry), timestamp_writes });
            pass.set_pipeline(pipe);
            pass.set_bind_group(0, &bg, &[]);
            if let Some(b) = &bg_ext {
                pass.set_bind_group(1, b, &[]);
            }
            pass.dispatch_workgroups(groups.0.max(1), groups.1.max(1), 1);
        }
        #[cfg(feature = "profile")]
        {
            self.timestamps = batch;
        }
        self.pending += 1;
        // Keep command buffers (and the transient textures they hold) bounded.
        if self.pending >= 256 {
            self.submit();
        }
    }

    /// Dispatch a dedicated texture layout while retaining profiling and batch bounds.
    pub(crate) fn custom_pixels(&mut self, entry: &str, pipe: &wgpu::ComputePipeline, bg: &wgpu::BindGroup, size: (u32, u32)) {
        #[cfg(feature = "profile")]
        let mut batch = self.timestamps.take().or_else(|| crate::timestamps::Batch::new(self.g));
        #[cfg(feature = "profile")]
        let timestamp_writes = batch.as_mut().and_then(|b| b.mark(entry));
        #[cfg(not(feature = "profile"))]
        let timestamp_writes = None;
        {
            let mut pass = self.encoder().begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some(entry), timestamp_writes });
            pass.set_pipeline(pipe);
            pass.set_bind_group(0, bg, &[]);
            pass.dispatch_workgroups(size.0.div_ceil(16), size.1.div_ceil(16), 1);
        }
        #[cfg(feature = "profile")]
        {
            self.timestamps = batch;
        }
        self.pending += 1;
        if self.pending >= 256 {
            self.submit();
        }
    }

    /// Per-pixel kernel over `out`'s size (16×16 workgroups).
    pub fn pixels(&mut self, entry: &str, p: &Params, src: &GpuImage, aux: Option<&GpuImage>, out: &GpuImage, data: Option<&wgpu::Buffer>) {
        let groups = (out.width.div_ceil(16), out.height.div_ceil(16));
        self.dispatch(entry, p, src, aux, out, data, groups);
    }

    /// Copy `src` into a larger zeroed image at (`x`, `y`) (Buf::pad).
    pub fn copy_into(&mut self, src: &GpuImage, dst: &GpuImage, x: u32, y: u32) {
        let enc = self.encoder();
        enc.copy_texture_to_texture(
            wgpu::TexelCopyTextureInfo { texture: &src.texture, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            wgpu::TexelCopyTextureInfo { texture: &dst.texture, mip_level: 0, origin: wgpu::Origin3d { x, y, z: 0 }, aspect: wgpu::TextureAspect::All },
            wgpu::Extent3d { width: src.width, height: src.height, depth_or_array_layers: 1 },
        );
    }

    /// Copy the `w`×`h` region of `src` at (`x`, `y`) into a new image (the caller checks
    /// that it lies inside `src`).
    pub fn crop(&mut self, src: &GpuImage, x: u32, y: u32, w: u32, h: u32) -> GpuImage {
        let out = self.scratch(w, h);
        let enc = self.encoder();
        enc.copy_texture_to_texture(
            wgpu::TexelCopyTextureInfo { texture: &src.texture, mip_level: 0, origin: wgpu::Origin3d { x, y, z: 0 }, aspect: wgpu::TextureAspect::All },
            wgpu::TexelCopyTextureInfo { texture: &out.texture, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
        out
    }

    pub(crate) fn keep_native(
        &mut self,
        owner: effectcraft_render::footage::SharedFrame,
        key: u64,
        token: effectcraft_render::cache::ResidentToken,
        metadata: Vec<u64>,
        image: NativeImage,
    ) {
        let Some(cache) = self.owner.as_ref() else { return };
        if self.g.deferred.is_some() || self.native_pending.len() >= 256 || token.identity() != cache.identity() {
            return;
        }
        let Some(allocation) = image.img.allocation.clone() else { return };
        if !allocation.generation().same(&self.g.generation) {
            return;
        }
        let epoch = self.epoch.clone();
        let Ok(pending) = cache.prepare(Arc::new(image), allocation, GpuStage::Blend, self.g.retained_cap()) else { return };
        if self.native_pending.try_reserve(1).is_ok() {
            self.native_pending.push(PendingNative { owner, key, token, metadata, cache: cache.clone(), pending, epoch });
        }
    }
    pub(crate) fn publish_native(&mut self) {
        if self.g.check_health().is_err() {
            self.native_pending.clear();
            self.resident_pending.clear();
            return;
        }
        for (pending, _flight) in self.resident_pending.drain(..) {
            if self.epoch.strong_count() == 0 {
                continue;
            }
            let Some(owner) = self.owner.as_ref() else { continue };
            let Ok(slot) = pending.publish() else { continue };
            let entry = SlotRef { owner: owner.clone(), slot, epoch: self.epoch.clone() };
            let mut u = match self.g.uploads.lock() {
                Ok(u) => u,
                Err(poison) => {
                    drop(poison);
                    entry.release();
                    continue;
                }
            };
            if self.epoch.strong_count() == 0
                || !self.g.generation.valid()
                || !self.g.generation.within_cap()
                || u.resident.len() >= 4096
                || u.resident.try_reserve(1).is_err()
            {
                drop(u);
                entry.release();
                continue;
            }
            u.resident.push(entry);
        }
        for entry in self.native_pending.drain(..) {
            self.g.native_keep(entry);
        }
    }
    pub(crate) fn upload_buf(&self, buf: &Arc<Buf>) -> Option<GpuImage> {
        self.g.upload_buf_owned(buf, self.owner.as_ref())
    }
    pub(crate) fn keep(&self, img: GpuImage) -> Option<u64> {
        self.g.keep_owned(img, self.owner.as_ref(), &self.epoch)
    }

    /// Submit recorded work.
    pub fn submit(&mut self) {
        #[allow(unused_mut)]
        if let Some((mut enc, id)) = self.enc.take() {
            #[cfg(feature = "profile")]
            let timing = self.timestamps.take().and_then(|b| b.resolve(self.g, &mut enc));
            self.submitted = Some(self.g.queue.submit([enc.finish()]));
            #[cfg(feature = "profile")]
            if let Some(timing) = timing {
                timing.map(self.g.queue.get_timestamp_period());
            }
            if let Ok(mut p) = self.g.pool.lock() {
                p.close(id);
            }
        }
        self.pending = 0;
    }

    /// Read an image back to the CPU (submits and waits). `None` where readback is impossible.
    pub fn download(&mut self, img: &GpuImage) -> Option<Image> {
        let layout = crate::readback::Layout::new(img.width, img.height, 16, self.g.device.limits().max_buffer_size)
            .map_err(|e| log::error!("gpu image readback: {e}"))
            .ok()?;
        let mut data: Vec<[f32; 4]> = Vec::new();
        data.try_reserve_exact(layout.len / 16).map_err(|e| log::error!("gpu image allocation: {e}")).ok()?;
        // Rows go straight from the mapped staging buffer into the image (no zero fill, no
        // intermediate copy).
        self.read_rows(&img.texture, img.width, img.height, 16, |r| match bytemuck::try_cast_slice::<u8, [f32; 4]>(r) {
            Ok(px) => data.extend_from_slice(px),
            Err(_) => data.extend(r.as_chunks::<16>().0.iter().map(|c| bytemuck::pod_read_unaligned::<[f32; 4]>(c))),
        })?;
        if data.len() != layout.len / 16 {
            log::error!("gpu image readback size mismatch");
            return None;
        }
        Some(Image { width: img.width, height: img.height, data })
    }

    /// Tightly packed texel bytes of a texture (`bpp` bytes per pixel).
    pub fn read_texture(&mut self, texture: &wgpu::Texture, w: u32, h: u32, bpp: u32) -> Option<Vec<u8>> {
        let layout =
            crate::readback::Layout::new(w, h, bpp, self.g.device.limits().max_buffer_size).map_err(|e| log::error!("gpu texture readback: {e}")).ok()?;
        let mut out = Vec::new();
        out.try_reserve_exact(layout.len).map_err(|e| log::error!("gpu readback allocation: {e}")).ok()?;
        self.read_rows(texture, w, h, bpp, |r| out.extend_from_slice(r))?;
        if out.len() != layout.len {
            log::error!("gpu texture readback size mismatch");
            return None;
        }
        Some(out)
    }

    /// [`Enc::read_texture`] row by row (`w × bpp` bytes each, top to bottom) through a reused
    /// staging buffer: `row` reads each straight from the mapped buffer.
    fn read_rows(&mut self, texture: &wgpu::Texture, w: u32, h: u32, bpp: u32, row: impl FnMut(&[u8])) -> Option<()> {
        let _profile = effectcraft_render::profile::span("readback");
        if !self.g.can_wait() {
            return None;
        }
        let layout =
            crate::readback::Layout::new(w, h, bpp, self.g.device.limits().max_buffer_size).map_err(|e| log::error!("gpu texture readback: {e}")).ok()?;
        self.g.transfers.count(false, layout.len);
        let padded = layout.stride;
        let size = layout.size;
        let buf = self
            .g
            .staging
            .lock()
            .ok()
            .and_then(|mut s| {
                let buf = s.get_mut(&size).and_then(Vec::pop);
                s.retain(|_, buffers| !buffers.is_empty());
                buf
            })
            .unwrap_or_else(|| {
                self.g.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("readback"),
                    size,
                    usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                })
            });
        self.encoder().copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo { texture, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            wgpu::TexelCopyBufferInfo { buffer: &buf, layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(padded), rows_per_image: Some(h) } },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
        self.submit();
        self.read_staging(&buf, layout.row as usize, padded as usize, h as usize, row).map_err(|e| log::error!("gpu texture readback: {e}")).ok()?;
        // Resized layers used to retain two buffers for every size ever read back (#591).
        if let Ok(mut s) = self.g.staging.lock() {
            let budget = self.g.auxiliary_budget.load(std::sync::atomic::Ordering::Relaxed);
            if size > budget {
                return Some(());
            }
            let retained = s.iter().map(|(size, buffers)| size.saturating_mul(buffers.len() as u64)).sum::<u64>();
            if retained.saturating_add(size) > budget {
                s.clear();
            }
            let v = s.entry(size).or_default();
            if v.len() < 2 {
                v.push(buf);
            }
        }
        Some(())
    }

    /// Wait for this encoder's latest submission only and hand the mapped rows of `buf` to `f`.
    #[cfg(not(target_arch = "wasm32"))]
    fn read_staging(&self, buf: &wgpu::Buffer, row: usize, stride: usize, height: usize, f: impl FnMut(&[u8])) -> Result<(), String> {
        self.g.read_mapped(buf, self.submitted.clone(), |view| crate::readback::for_rows(view, row, stride, height, f))
    }

    #[cfg(target_arch = "wasm32")]
    fn read_staging(&self, _buf: &wgpu::Buffer, _row: usize, _stride: usize, _height: usize, _f: impl FnMut(&[u8])) -> Result<(), String> {
        Err("synchronous GPU readback is unavailable in the browser".into())
    }

    /// [`Enc::read_texture`] without waiting for the GPU (the browser's main thread can't): `done`
    /// gets the bytes once the copy is mapped (from the browser's event loop on the web; from
    /// a later device poll natively).
    pub fn read_texture_async(&mut self, texture: &wgpu::Texture, w: u32, h: u32, bpp: u32, done: impl FnOnce(Option<Vec<u8>>) + wgpu::WasmNotSend + 'static) {
        if let Err(e) = self.g.readbacks.check() {
            log::error!("gpu readback: {e}");
            crate::readback::reject(done, e);
            return;
        }
        let layout = match crate::readback::Layout::new(w, h, bpp, self.g.device.limits().max_buffer_size) {
            Ok(layout) => layout,
            Err(e) => {
                log::error!("gpu texture readback: {e}");
                crate::readback::reject(done, e);
                return;
            }
        };
        let row = layout.row;
        let padded = layout.stride;
        let buf = self.g.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("readback-async"),
            size: layout.size,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        self.encoder().copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo { texture, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            wgpu::TexelCopyBufferInfo { buffer: &buf, layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(padded), rows_per_image: Some(h) } },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
        self.submit();
        self.g.map_readback(&buf, row as usize, padded as usize, h as usize, move |r| {
            done(r.map_err(|e| log::error!("gpu texture readback: {e}")).ok());
        });
        #[cfg(not(target_arch = "wasm32"))]
        let _ = self.g.poll_readbacks(wgpu::PollType::Poll);
    }

    /// A deferred readback of `img` (RGBA f32) under `key` (see [`crate::deferred`]): the bytes
    /// once they arrived (`Some(None)`: the readback failed), else `None` after starting the
    /// copy if needed and marking the pass missed.
    pub(crate) fn read_keyed(&mut self, img: &GpuImage, key: u128, offset: [f64; 2], scale: f64) -> Option<Option<crate::deferred::Readback>> {
        use crate::deferred::Lookup;
        let d = self.g.deferred.clone()?;
        match d.lookup(key) {
            Lookup::Ready(r) => return Some(r),
            Lookup::InFlight => {}
            Lookup::Absent => {
                self.g.transfers.count(false, img.width as usize * img.height as usize * 16);
                let done = d.start(key, img.width, img.height, offset, scale);
                self.read_texture_async(&img.texture, img.width, img.height, 16, done);
            }
        }
        d.miss();
        None
    }

    /// A data buffer for kernels that read tables (curves).
    pub fn data(&self, v: &[f32]) -> wgpu::Buffer {
        let mut bytes = Vec::with_capacity(v.len().max(4) * 4);
        for x in v {
            bytes.extend_from_slice(&x.to_le_bytes());
        }
        self.bytes(bytes)
    }

    /// Copy an image into a storage buffer (RGBA f32 rows) for kernels that read a third image
    /// through `data`: (buffer, row length in pixels).
    pub fn image_rows(&mut self, img: &GpuImage) -> (wgpu::Buffer, u32) {
        let row = (img.width * 16).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let buf = self.g.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("image rows"),
            size: row as u64 * img.height as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        self.encoder().copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo { texture: &img.texture, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            wgpu::TexelCopyBufferInfo {
                buffer: &buf,
                layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(row), rows_per_image: Some(img.height) },
            },
            wgpu::Extent3d { width: img.width, height: img.height, depth_or_array_layers: 1 },
        );
        (buf, row / 16)
    }

    /// A read-only storage buffer holding `bytes` (padded to 16 bytes).
    pub fn bytes(&self, mut bytes: Vec<u8>) -> wgpu::Buffer {
        while bytes.len() < 16 || !bytes.len().is_multiple_of(4) {
            bytes.push(0);
        }
        self.g.buffer_init(Some("data"), &bytes, wgpu::BufferUsages::STORAGE)
    }

    /// Convert to the viewer's premultiplied RGBA8 texture (egui-wgpu's native texture format),
    /// with a full mip chain so the viewer minifies it without aliasing (#417).
    pub fn display(&mut self, img: &GpuImage) -> wgpu::Texture {
        let g = self.g;
        let levels = mip_levels(img.width, img.height);
        let tex = g.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("viewer frame"),
            size: wgpu::Extent3d { width: img.width, height: img.height, depth_or_array_layers: 1 },
            mip_level_count: levels,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let level = |l: u32| tex.create_view(&wgpu::TextureViewDescriptor { base_mip_level: l, mip_level_count: Some(1), ..Default::default() });
        // Level 0 from the frame, then each level the 2 × 2 mean of the one above.
        let mut steps = vec![(&g.display, img.view.clone(), level(0), img.width, img.height)];
        for l in 1..levels {
            steps.push((&g.display_mip, level(l - 1), level(l), (img.width >> l).max(1), (img.height >> l).max(1)));
        }
        #[cfg(feature = "profile")]
        let mut batch = self.timestamps.take().or_else(|| crate::timestamps::Batch::new(g));
        let enc = self.encoder();
        for (step, (pipeline, src, dst, w, h)) in steps.iter().enumerate() {
            let entry = if step == 0 { "display" } else { "display_mip" };
            #[cfg(feature = "profile")]
            let timestamp_writes = batch.as_mut().and_then(|b| b.mark(entry));
            #[cfg(not(feature = "profile"))]
            let timestamp_writes = None;
            let bg = g.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: None,
                layout: &g.display_bgl,
                entries: &[
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(src) },
                    wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(dst) },
                ],
            });
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some(entry), timestamp_writes });
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, &bg, &[]);
            pass.dispatch_workgroups(w.div_ceil(16), h.div_ceil(16), 1);
        }
        #[cfg(feature = "profile")]
        {
            self.timestamps = batch;
        }
        tex
    }
}

/// Mip levels of a full chain for a `w × h` texture (down to 1 × 1).
pub(crate) fn mip_levels(w: u32, h: u32) -> u32 {
    u32::BITS - w.max(h).max(1).leading_zeros()
}

#[cfg(test)]
mod upload_budget_tests {
    use super::*;

    #[test]
    fn pool_encoder_local_reuse_keeps_cross_encoder_barrier() {
        let Some(g) = crate::tests::gpu() else { return };
        let mut pool = Pool::default();
        let first = pool.open();
        pool.release(g.ctx.texture(8, 8));
        assert!(pool.take_for_encoder(8, 8, None).is_none());
        let second = pool.open();
        assert!(pool.take_for_encoder(8, 8, Some(first)).is_none());
        assert!(pool.take_for_encoder(8, 8, Some(second)).is_none());
        pool.close(second);
        assert!(pool.take_for_encoder(8, 8, Some(first)).is_some());
        assert_eq!(pool.released_bytes, 0);
        pool.release(g.ctx.texture(8, 8));
        pool.clear();
        assert!(pool.take_for_encoder(8, 8, Some(first)).is_none(), "purged releases cannot reappear");
    }

    #[test]
    fn pool_encoder_local_reuse_preserves_prior_reads() {
        crate::tests::hold_gpu_lock();
        let g = GpuContext::headless();
        assert!(g.is_some() || std::env::var("EFFECTCRAFT_REQUIRE_GPU").as_deref() != Ok("1"));
        let Some(g) = g else { return };
        let mut e = Enc::new(&g);
        let source = e.image(8, 8);
        e.fill(&source, 0.25);
        let saved = e.scratch(8, 8);
        e.copy_into(&source, &saved, 0, 0);
        drop(source);
        let before = g.pool_stats();
        let reused = e.scratch(8, 8);
        assert_eq!(g.pool_stats().0, before.0 + 1, "reuse before submit");
        e.fill(&reused, 0.75);
        let prior = e.download(&saved).unwrap();
        let later = e.download(&reused).unwrap();
        assert!(prior.data.iter().flatten().all(|v| *v == 0.25));
        assert!(later.data.iter().flatten().all(|v| *v == 0.75));
    }

    #[test]
    fn pool_encoder_local_reuse_reverse_submission_and_rollover() {
        crate::tests::hold_gpu_lock();
        let g = GpuContext::headless();
        assert!(g.is_some() || std::env::var("EFFECTCRAFT_REQUIRE_GPU").as_deref() != Ok("1"));
        let Some(g) = g else { return };
        let mut first = Enc::new(&g);
        let source = first.image(8, 8);
        first.fill(&source, 0.25);
        first.submit();
        let saved = first.scratch(8, 8);
        first.copy_into(&source, &saved, 0, 0);
        drop(source);
        let mut second = Enc::new(&g);
        let other = second.scratch(8, 8);
        second.fill(&other, 0.75);
        second.submit();
        assert!(first.download(&saved).unwrap().data.iter().flatten().all(|v| *v == 0.25), "later encoder submitted first cannot overwrite pending input");
        let mut current = saved;
        for _ in 0..260 {
            let next = first.scratch(8, 8);
            first.copy_into(&current, &next, 0, 0);
            current = next;
        }
        assert!(first.download(&current).unwrap().data.iter().flatten().all(|v| *v == 0.25), "ring rollover preserves reads");
    }

    #[test]
    fn pool_size_transition_reuses_small_textures_without_touching_pending() {
        let Some(g) = crate::tests::gpu() else { return };
        let mut pool = Pool::default();
        pool.set_budget(8 * 8 * 16);
        pool.release(g.ctx.texture(8, 8));
        assert!(pool.take(4, 4).is_none());
        assert_eq!(pool.bytes, 0, "large free texture makes room for new size");
        let encoder = pool.open();
        pool.release(g.ctx.texture(4, 4));
        assert_eq!(pool.released.len(), 1);
        assert!(pool.take(4, 4).is_none(), "unsubmitted texture is not reused");
        assert_eq!(pool.released.len(), 1, "pending resources are not evicted");
        pool.close(encoder);
        assert!(pool.take(4, 4).is_some(), "new preview size now reuses its allocation");
        pool.release(g.ctx.texture(4, 4));
        pool.release(g.ctx.texture(4, 4));
        pool.release(g.ctx.texture(4, 4));
        assert!(pool.take(6, 4).is_none());
        assert_eq!(pool.bytes, 3 * 4 * 4 * 16, "collective pressure preserves useful smaller sizes");
        assert_eq!(pool.free.get(&(4, 4)).map(Vec::len), Some(3));
    }

    #[test]
    fn pool_pending_reuse_is_bounded_and_purge_cannot_repopulate_it() {
        crate::tests::hold_gpu_lock();
        let context = GpuContext::headless();
        assert!(context.is_some() || !std::env::var("EFFECTCRAFT_REQUIRE_GPU").is_ok_and(|v| v == "1"), "required GPU unavailable");
        let Some(g) = context else { return };
        let mut pool = Pool::default();
        pool.set_budget(2 * 8 * 8 * 16);
        let encoder = pool.open();
        for _ in 0..8 {
            pool.release(g.texture(8, 8));
        }
        assert_eq!(pool.released.len(), 2);
        assert_eq!(pool.released_bytes, 2 * 8 * 8 * 16);
        pool.set_budget(8 * 8 * 16);
        assert_eq!(pool.released.len(), 1);
        pool.clear();
        pool.close(encoder);
        assert_eq!(pool.bytes, 0);
        assert_eq!(pool.released_bytes, 0);
        assert!(pool.free.is_empty());
    }

    /// Kept effect results awaiting adoption count against the upload budget: an oversized
    /// one is not kept, older ones and uploads make room, and a lower budget drops them.
    #[test]
    fn kept_effect_results_count_against_the_upload_budget() {
        crate::tests::hold_gpu_lock();
        let context = GpuContext::headless();
        assert!(context.is_some() || !std::env::var("EFFECTCRAFT_REQUIRE_GPU").is_ok_and(|v| v == "1"), "required GPU unavailable");
        let Some(g) = context else { return };
        let cache = effectcraft_render::LayerCache::new(UPLOAD_BUDGET);
        let owner = cache.gpu_owner().unwrap();
        let one = 8 * 8 * 16;
        g.set_cache_budget(2 * one);
        assert!(g.keep_owned(g.image(16, 16), Some(&owner), &g.upload_epoch()).is_none(), "larger than the budget");
        let buf = Arc::new(Buf { img: Image::new(8, 8), offset: [0.0; 2], scale: 1.0 });
        assert!(g.upload_buf_owned(&buf, Some(&owner)).is_some());
        let a = g.keep_owned(g.image(8, 8), Some(&owner), &g.upload_epoch()).unwrap();
        assert_eq!((g.kept_count(), g.upload_stats()), (1, (1, one)));
        let b = g.keep_owned(g.image(8, 8), Some(&owner), &g.upload_epoch()).unwrap();
        assert_eq!((g.kept_count(), g.upload_stats()), (2, (0, 0)), "the upload makes room");
        let _c = g.keep_owned(g.image(8, 8), Some(&owner), &g.upload_epoch()).unwrap();
        assert_eq!(g.kept_count(), 2, "the oldest kept result makes room");
        g.adopt(a, &buf);
        assert_eq!(g.upload_stats(), (0, 0), "the dropped ticket adopts nothing");
        g.set_cache_budget(one);
        assert_eq!(g.kept_count(), 1);
        g.adopt(b, &buf);
        assert_eq!(g.upload_stats(), (0, 0));
        g.set_cache_budget(0);
        assert_eq!(g.kept_count(), 0);
        g.set_cache_budget(UPLOAD_BUDGET);
        let _d = g.keep_owned(g.image(8, 8), Some(&owner), &g.upload_epoch()).unwrap();
        g.clear_uploads();
        assert_eq!(g.kept_count(), 0);
    }

    #[test]
    fn shared_memory_cache_budgets_follow_preferences_and_have_lower_caps() {
        assert_eq!(cache_budgets(0, true), (0, 0, 0));
        assert_eq!(cache_budgets(64 << 20, true), (64 << 20, 32 << 20, 4 << 20));
        assert_eq!(cache_budgets(usize::MAX, true), (256 << 20, 128 << 20, 32 << 20));
        assert_eq!(cache_budgets(usize::MAX, false), (UPLOAD_BUDGET, POOL_BUDGET, AUX_CACHE_BUDGET));
    }

    #[test]
    fn auxiliary_caches_are_bounded_across_sizes_and_purged() {
        crate::tests::hold_gpu_lock();
        let context = GpuContext::headless();
        assert!(context.is_some() || !std::env::var("EFFECTCRAFT_REQUIRE_GPU").is_ok_and(|v| v == "1"), "required GPU unavailable");
        let Some(g) = context else { return };
        // A tiny budget makes changing layer extents exercise eviction with kilobytes.
        g.set_cache_budget(16 * 4096);
        for width in 8..40 {
            let mut enc = Enc::new(&g);
            let zero = enc.zeros(width, 8);
            let pixels = enc.download(&zero).unwrap();
            assert!(pixels.data.iter().all(|p| *p == [0.0; 4]));
            let retained = g.zeros.lock().unwrap().values().map(|img| tex_bytes(&img.texture)).sum::<usize>();
            assert!(retained <= 4096, "zero cache retained {retained} bytes");
            let staging = g.staging.lock().unwrap();
            let retained = staging.iter().map(|(size, buffers)| size * buffers.len() as u64).sum::<u64>();
            assert!(retained <= 4096, "staging cache retained {retained} bytes");
            assert!(staging.values().all(|buffers| !buffers.is_empty()));
        }
        g.clear_uploads();
        assert!(g.zeros.lock().unwrap().is_empty());
        assert!(g.staging.lock().unwrap().is_empty());
        g.set_cache_budget(0);
        let mut enc = Enc::new(&g);
        let zero = enc.zeros(8, 8);
        assert!(enc.download(&zero).is_some());
        assert!(g.zeros.lock().unwrap().is_empty());
        assert!(g.staging.lock().unwrap().is_empty());
    }

    /// Readback copies rows straight out of the padded staging buffer: an odd width (row
    /// padding) round-trips bit-exactly through `download` and `read_texture`.
    #[test]
    fn readback_round_trips_padded_rows() {
        crate::tests::hold_gpu_lock();
        let Some(g) = GpuContext::headless() else { return };
        let (w, h) = (7, 5);
        let data: Vec<[f32; 4]> = (0..w * h).map(|i| [i as f32, -0.5 * i as f32, f32::MIN_POSITIVE * i as f32, 1.0 / (i as f32 + 3.0)]).collect();
        let img = Image { width: w, height: h, data };
        let up = g.upload_image(&img).unwrap();
        let mut e = Enc::new(&g);
        let down = e.download(&up).unwrap();
        assert_eq!((down.width, down.height), (w, h));
        assert_eq!(bytemuck::cast_slice::<_, u8>(&down.data), bytemuck::cast_slice::<_, u8>(&img.data));
        let bytes = e.read_texture(&up.texture, w, h, 16).unwrap();
        assert_eq!(bytes, bytemuck::cast_slice::<_, u8>(&img.data));
    }

    /// Every dispatch reads its own uniform ring slot, also across the automatic submit after
    /// [`RING_SLOTS`] dispatches and when a dropped encoder's ring is reused.
    #[test]
    fn uniform_ring_slots_keep_each_dispatch_params() {
        crate::tests::hold_gpu_lock();
        let Some(g) = GpuContext::headless() else { return };
        for _ in 0..2 {
            let mut e = Enc::new(&g);
            let n = RING_SLOTS + 44;
            let imgs: Vec<GpuImage> = (0..n)
                .map(|i| {
                    let img = GpuImage::new(g.texture(2, 2), 2, 2);
                    e.fill(&img, i as f32);
                    img
                })
                .collect();
            for (i, img) in imgs.iter().enumerate() {
                let px = e.download(img).unwrap();
                assert!(px.data.iter().all(|p| *p == [i as f32; 4]), "dispatch {i}: {:?}", px.data);
            }
        }
        assert!(g.uniforms.lock().unwrap().len() <= RINGS_KEPT);
    }

    /// A [`Params`] block fills exactly one uniform ring slot's binding.
    #[test]
    fn params_fill_one_ring_slot() {
        assert_eq!(Params::default().bytes().len() as u64, PARAMS_SIZE);
    }

    fn buf(w: u32) -> Arc<Buf> {
        Arc::new(Buf { img: Image::filled(w, 8, [0.5, 0.25, 0.125, 0.5]), offset: [0.0; 2], scale: 1.0 })
    }

    /// Textures of layer buffers that are gone are dropped at the next upload, not only once the
    /// upload budget overflows; the budget follows the layer cache's.
    #[test]
    fn dead_uploads_are_purged_and_budget_follows_prefs() {
        crate::tests::hold_gpu_lock();
        let context = GpuContext::headless();
        assert!(context.is_some() || !std::env::var("EFFECTCRAFT_REQUIRE_GPU").is_ok_and(|v| v == "1"), "required GPU unavailable");
        let Some(g) = context else { return };
        eprintln!("retained upload adapter {}", g.name);
        let cache = effectcraft_render::LayerCache::new(UPLOAD_BUDGET);
        let owner = cache.gpu_owner().unwrap();
        let a = buf(8);
        let b = buf(16);
        assert!(g.upload_buf_owned(&a, Some(&owner)).is_some() && g.upload_buf_owned(&b, Some(&owner)).is_some());
        assert_eq!(g.upload_stats(), (2, (8 + 16) * 8 * 16));
        drop(a);
        let c = buf(4);
        assert!(g.upload_buf_owned(&c, Some(&owner)).is_some());
        assert_eq!(g.upload_stats(), (2, (16 + 4) * 8 * 16), "a's texture dropped");
        // A budget below b + c keeps the most recently used.
        g.set_cache_budget(4 * 8 * 16 + 100);
        assert_eq!(g.upload_stats(), (1, 4 * 8 * 16));
        // Re-uploading the same live buffer is a hit.
        let ups = g.transfer_stats().uploads;
        assert!(g.upload_buf_owned(&c, Some(&owner)).is_some());
        assert_eq!(g.transfer_stats().uploads, ups);
        drop(b);
    }
    #[test]
    fn retained_indexes_drop_outside_lock_and_fan_out_to_two_owners() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        struct Probe {
            index: Weak<Mutex<Uploads>>,
            cache: Weak<effectcraft_render::LayerCache>,
            drops: Arc<AtomicUsize>,
        }
        impl Drop for Probe {
            fn drop(&mut self) {
                if let Some(index) = self.index.upgrade() {
                    assert!(index.try_lock().is_ok(), "index lock held during backend drop");
                }
                if let Some(cache) = self.cache.upgrade() {
                    let _ = cache.retained_stats();
                }
                self.drops.fetch_add(1, Ordering::Relaxed);
            }
        }
        let index = Arc::new(Mutex::new(Uploads::default()));
        let generation = effectcraft_render::allocation::GpuGeneration::default();
        let drops = Arc::new(AtomicUsize::new(0));
        let a = Arc::new(effectcraft_render::LayerCache::new(64));
        let b = Arc::new(effectcraft_render::LayerCache::new(64));
        let pixel = Arc::new(Buf { img: Image::new(1, 1), offset: [0.0; 2], scale: 1.0 });
        for (key, cache) in [(1, &a), (2, &b)] {
            cache.insert(key, pixel.clone());
            let owner = cache.gpu_owner().unwrap();
            let payload = Arc::new(Probe { index: Arc::downgrade(&index), cache: Arc::downgrade(cache), drops: drops.clone() });
            let allocation = effectcraft_render::allocation::GpuAllocation::rgba_f32(1, 1, generation.clone()).unwrap();
            let slot = owner.prepare(payload, allocation, GpuStage::CpuMirror, 64).unwrap().publish().unwrap();
            let mut u = index.lock().unwrap();
            let epoch = Arc::downgrade(&u.epoch);
            u.map.insert((owner.identity(), key as usize), Upload { buf: Arc::downgrade(&pixel), slot: SlotRef { owner, slot, epoch } });
        }
        generation.retire();
        clear_retained(&index);
        assert_eq!(drops.load(Ordering::Relaxed), 2);
        assert!(a.get(1).is_some() && b.get(2).is_some());
        assert_eq!(a.retained_stats().live_bytes, 16);
        assert_eq!(b.retained_stats().live_bytes, 16);
    }
    #[test]
    fn ordinary_resident_device_failure_rejects_readback_and_preserves_materialized_cpu() {
        crate::tests::hold_gpu_lock();
        let context = GpuContext::headless();
        assert!(context.is_some() || !std::env::var("EFFECTCRAFT_REQUIRE_GPU").is_ok_and(|v| v == "1"), "required GPU unavailable");
        let Some(context) = context else { return };
        let gpu = crate::Gpu::from_context(context);
        eprintln!("ordinary failure adapter {}", gpu.ctx.name);
        let mut scene = crate::tests::Scene::new(effectcraft_project::BitDepth::Bpc32);
        let layer = scene.solid([0.3, 0.5, 0.7], 20, 15);
        let id = scene.push(layer);
        let cache = effectcraft_render::LayerCache::default();
        let mut renderer = effectcraft_render::Renderer::new(
            &scene.p,
            &crate::tests::Pattern,
            effectcraft_render::RenderOpts { backend: effectcraft_render::Backend::Gpu, ..Default::default() },
        );
        renderer.accel = Some(&gpu);
        renderer.cache = Some(&cache);
        gpu.within_memory(|| gpu.render_display(&renderer, scene.cid, effectcraft_time::Tick::ZERO)).unwrap().unwrap();
        let ctx = renderer.eval_ctx(scene.cid, effectcraft_time::Tick::ZERO).unwrap();
        let layer = ctx.comp.layers.iter().find(|l| l.id == id).unwrap();
        let key = renderer.resident_content_key(&ctx, layer).unwrap();
        let slot = cache.gpu_owner().unwrap().resident(key, GpuStage::Processed).unwrap();
        let cpu = cache.get(key).unwrap();
        let before = gpu.ctx.transfer_stats();
        gpu.ctx.device_failure_notifier()("synthetic ordinary-resident device failure".into());
        assert!(effectcraft_render::cache::ResidentReadback::readback(&LazyReadback(Arc::downgrade(&gpu.ctx)), &slot).is_none());
        assert_eq!(gpu.ctx.transfer_stats().readbacks, before.readbacks, "failed device performs no readback allocation/map");
        assert!(Arc::ptr_eq(&cpu, &cache.get(key).unwrap()));
        assert!(cache.gpu_owner().unwrap().resident(key, GpuStage::Processed).is_none());
        assert!(gpu.render_display(&renderer, scene.cid, effectcraft_time::Tick::ZERO).is_none());
        let fallback = renderer.comp_frame(scene.cid, effectcraft_time::Tick::ZERO);
        let oracle =
            effectcraft_render::Renderer::new(&scene.p, &crate::tests::Pattern, Default::default()).comp_frame_cpu(scene.cid, effectcraft_time::Tick::ZERO);
        assert_eq!(fallback, oracle, "public renderer falls back to the exact CPU frame after device failure");
        assert_eq!(gpu.ctx.transfer_stats().readbacks, before.readbacks, "fallback does not retry failed-device readback");
        drop(slot);
        assert_eq!(cache.retained_stats().live_bytes, cpu.img.data.capacity() * std::mem::size_of::<[f32; 4]>());
    }

    #[test]
    fn retained_gpu_identity_clear_and_device_failure_preserve_cpu() {
        crate::tests::hold_gpu_lock();
        let context = GpuContext::headless();
        assert!(context.is_some() || !std::env::var("EFFECTCRAFT_REQUIRE_GPU").is_ok_and(|v| v == "1"), "required GPU unavailable");
        let Some(g) = context else { return };
        eprintln!("retained ownership adapter {}", g.name);
        let a = effectcraft_render::LayerCache::new(256);
        let b = effectcraft_render::LayerCache::new(256);
        let ao = a.gpu_owner().unwrap();
        let bo = b.gpu_owner().unwrap();
        let pixels = Arc::new(Buf { img: Image::new(1, 1), offset: [0.0; 2], scale: 1.0 });
        a.insert(1, pixels.clone());
        b.insert(2, pixels.clone());
        let first = g.upload_buf_owned(&pixels, Some(&ao)).unwrap();
        let uploads = g.transfer_stats().uploads;
        let alias = g.upload_buf_owned(&pixels, Some(&ao)).unwrap();
        assert!(Arc::ptr_eq(first.allocation.as_ref().unwrap(), alias.allocation.as_ref().unwrap()));
        assert_eq!(g.transfer_stats().uploads, uploads);
        assert_eq!(g.image(0, 0).allocation.as_ref().unwrap().bytes(), 16);
        assert_eq!(GpuImage::new(g.texture(2, 2), 100, 100).allocation.as_ref().unwrap().bytes(), 64);
        drop(first);
        drop(alias);
        g.clear_uploads();
        assert_eq!(g.upload_stats(), (0, 0));
        assert!(g.upload_buf_owned(&pixels, Some(&ao)).is_some());
        assert!(g.upload_buf_owned(&pixels, Some(&bo)).is_some());
        assert_eq!(g.upload_stats(), (2, 32));
        g.device_failure_notifier()("synthetic retained ownership device failure".to_owned());
        assert!(g.check_health().is_err());
        assert_eq!(g.upload_stats(), (0, 0));
        assert!(a.get(1).is_some() && b.get(2).is_some());
        assert_eq!((a.retained_stats().live_bytes, b.retained_stats().live_bytes), (16, 16));
    }
}
