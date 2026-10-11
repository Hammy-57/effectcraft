//! Background frame rendering and the RAM preview cache.
//!
//! Frames render on a dedicated thread pool from immutable `Arc<Project>` snapshots, so the UI
//! never waits for the compositor. Results land in a memory-budgeted cache keyed by the comp's
//! *content* ([`comp_content`]: the comp and everything it uses), frame, scale, view and render
//! options; the timeline draws the cached range as the green cache bar. An edit keeps the frames
//! of every comp it doesn't touch, and undo finds the frames of the state it returns to.
//! Switches that don't change pixels (Audio, Lock, Shy, Hide Shy Layers) keep the frames too.
//! When the budget is full the least recently shown frames go first.
//!
//! wasm32 has no threads: jobs wait in the same priority queue and [`Frames::pump`] hands them
//! to a [`RemoteFrames`] (the browser's frame worker, a second engine instance fed with project
//! diffs, `effectcraft_engine::remote`) or, without one, renders them on the UI thread between
//! egui frames, within a time budget.
//!
//! With the disk cache on (Settings ▸ Media & Disk Cache), every CPU-rendered frame is also
//! written to disk under a content key (the comp's content hash, frame, scale, view and render
//! options), and a job first looks there: frames survive restarts and project reopenings. The
//! timeline shows disk-only frames as a blue cache bar.
//!
//! With Mercury GPU Acceleration (a [`Gpu`] on egui-wgpu's device and the project's renderer set
//! to the GPU) frames stay on the GPU: the compositor writes an RGBA8 texture that the viewer
//! registers with egui-wgpu and draws directly; pixels are read back only when something needs
//! them (Info panel, eyedroppers, histograms).

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use effectcraft_engine::project::{Comp, ItemId, ItemKind, LayerSource, Node, Project, PropGroup};
use effectcraft_engine::render::disk_cache::{self, DiskCache};
use effectcraft_engine::render::{ExprHost, FootageSource, LayerCache, RenderOpts, Renderer};
use effectcraft_engine::time::Tick;
use effectcraft_gpu::{DisplayFrame, Gpu};
use rayon::prelude::*;

/// Comp identities (and the project snapshots behind them) the RAM preview keeps frames of.
const MAX_KEEPERS: usize = 64;

/// GPU frames kept for RAM preview (bytes of video memory) at first. wgpu can't tell how much
/// video memory there is: each time the device runs out, the budget halves (to
/// [`MIN_GPU_BUDGET`]) and the frame renders on the CPU (#106).
const GPU_BUDGET: usize = 1 << 30;
const MIN_GPU_BUDGET: usize = 64 << 20;

/// A rendered viewer frame: CPU pixels or a GPU texture.
#[derive(Clone)]
pub enum FrameImage {
    Cpu(Arc<egui::ColorImage>),
    Gpu(Arc<DisplayFrame>),
}

impl FrameImage {
    pub fn size(&self) -> [usize; 2] {
        match self {
            FrameImage::Cpu(c) => c.size,
            FrameImage::Gpu(f) => [f.width as usize, f.height as usize],
        }
    }
    fn bytes(&self) -> usize {
        self.size()[0] * self.size()[1] * 4
    }
    fn is_gpu(&self) -> bool {
        matches!(self, FrameImage::Gpu(_))
    }
}

/// A CPU frame's pixels as premultiplied RGBA bytes, shared with the RAM cache (`Color32` is
/// premultiplied RGBA already): what a frame worker hands the disk cache's writer thread.
struct ColorBytes(Arc<egui::ColorImage>);

impl AsRef<[u8]> for ColorBytes {
    fn as_ref(&self) -> &[u8] {
        self.0.as_raw()
    }
}

/// A viewer frame: its comp's content, the frame, scale, view and render options. `revision` is
/// the project revision it was asked for at (job bookkeeping); it is not part of the identity, so
/// a later revision with the same content finds the frame.
#[derive(Clone, Copy, Debug)]
pub struct FrameKey {
    pub revision: u64,
    /// The comp's content key ([`Frames::content_of`]).
    pub content: u64,
    pub comp: u64,
    pub frame: i64,
    /// Render scale × 1000.
    pub scale: u32,
    /// Hash of the 3D view camera (0 = the comp's active camera).
    pub view: u64,
    /// Hash of the other render options (Draft / Fast Previews, shadows, nested switches…).
    pub opts: u64,
}

impl FrameKey {
    /// Everything but the revision.
    fn id(&self) -> (u64, u64, i64, u32, u64, u64) {
        (self.content, self.comp, self.frame, self.scale, self.view, self.opts)
    }

    /// Whether `self` is a frame of the same comp content, scale, view and options as `base`.
    pub fn same_series(&self, base: &FrameKey) -> bool {
        FrameKey { frame: base.frame, ..*self } == *base
    }
}

impl PartialEq for FrameKey {
    fn eq(&self, o: &FrameKey) -> bool {
        self.id() == o.id()
    }
}

impl Eq for FrameKey {}

impl std::hash::Hash for FrameKey {
    fn hash<H: std::hash::Hasher>(&self, h: &mut H) {
        self.id().hash(h);
    }
}

struct Cache {
    map: HashMap<FrameKey, FrameImage>,
    order: VecDeque<FrameKey>,
    protected: HashMap<FrameKey, usize>,
    bytes: usize,
    /// Bytes of GPU frames (also counted in `bytes`).
    gpu_bytes: usize,
    budget: usize,
    /// Bytes of GPU frames to keep at most ([`GPU_BUDGET`]).
    gpu_budget: usize,
    /// Changes whenever a frame comes or goes (see [`Frames::disk_frames`]).
    generation: u64,
}

impl Cache {
    fn insert(&mut self, k: FrameKey, img: FrameImage) {
        self.generation = self.generation.wrapping_add(1);
        let (sz, gpu) = (img.bytes(), img.is_gpu());
        if let Some(old) = self.map.insert(k, img) {
            self.forget(&old);
        } else {
            self.order.push_back(k);
        }
        self.bytes += sz;
        if gpu {
            self.gpu_bytes += sz;
        }
        self.evict_to_budget();
        self.evict_gpu();
    }
    /// Video memory: drop the oldest GPU frames past their own budget.
    fn evict_gpu(&mut self) {
        while self.gpu_bytes > self.gpu_budget {
            let Some(i) = self.eviction_index(true) else { break };
            if let Some(old) = self.order.remove(i) {
                self.remove(&old);
            }
        }
    }
    /// The GPU ran out of memory: keep half as many GPU frames from now on.
    fn gpu_out_of_memory(&mut self) {
        self.gpu_budget = (self.gpu_budget / 2).max(MIN_GPU_BUDGET);
        self.evict_gpu();
    }
    fn forget(&mut self, img: &FrameImage) {
        self.bytes -= img.bytes();
        if img.is_gpu() {
            self.gpu_bytes -= img.bytes();
        }
    }
    fn remove(&mut self, k: &FrameKey) {
        if let Some(i) = self.map.remove(k) {
            self.generation = self.generation.wrapping_add(1);
            self.forget(&i);
        }
    }
    /// Shown again: last in line for eviction.
    fn touch(&mut self, k: &FrameKey) {
        if let Some(i) = self.order.iter().rposition(|o| o == k)
            && let Some(k) = self.order.remove(i)
        {
            self.order.push_back(k);
        }
    }
    /// Drop every frame of the identities in `gone`.
    fn drop_contents(&mut self, gone: &HashSet<u64>) {
        let keys: Vec<FrameKey> = self.map.keys().filter(|k| gone.contains(&k.content)).copied().collect();
        for k in &keys {
            self.remove(k);
        }
        self.order.retain(|k| !gone.contains(&k.content));
    }
    fn eviction_index(&self, gpu_only: bool) -> Option<usize> {
        let eligible = |k: &FrameKey| !gpu_only || self.map.get(k).is_some_and(FrameImage::is_gpu);
        self.order.iter().position(|k| eligible(k) && !self.protected.contains_key(k)).or_else(|| {
            // If the window itself does not fit, sacrifice its furthest frame, not the playhead.
            self.order.iter().enumerate().filter(|(_, k)| eligible(k)).max_by_key(|(_, k)| self.protected.get(*k).copied().unwrap_or(0)).map(|(i, _)| i)
        })
    }

    fn evict_to_budget(&mut self) {
        while self.bytes > self.budget {
            let Some(i) = self.eviction_index(false) else { break };
            let Some(old) = self.order.remove(i) else { break };
            self.remove(&old);
        }
    }
}

/// What a render job needs (all cheap clones).
#[derive(Clone)]
pub struct RenderSource {
    pub project: Arc<Project>,
    pub footage: Arc<dyn FootageSource>,
    pub expr: Option<Arc<dyn ExprHost>>,
    pub layer_cache: Arc<LayerCache>,
    /// GPU compositor (Mercury GPU Acceleration), if available.
    pub gpu: Option<Gpu>,
    /// Keep GPU frames on the GPU for the viewer (native egui-wgpu textures). Off on the
    /// desktop: Show Channel / exposure / region-of-interest drawing read the CPU frame texture,
    /// so GPU frames are read back (the compositing still runs on the GPU). On in the browser
    /// while the viewer draws plainly, because there frames can't be read back synchronously
    /// (`EffectcraftApp::gpu_display`).
    pub gpu_display: bool,
    /// The persistent disk cache, when enabled.
    pub disk: Option<Arc<DiskCache>>,
}

/// A [`comp_content`] identity and, when it was found in another project, that project
/// ([`Frames::identity`]).
type Identity = (u64, Option<Arc<Project>>);

/// Content keys of (project revision, comp), computed once per revision.
type ContentKeys = Arc<Mutex<HashMap<(u64, u64), u128>>>;

fn content_key(keys: &ContentKeys, project: &Project, revision: u64, comp: u64) -> u128 {
    if let Some(k) = keys.lock().ok().and_then(|m| m.get(&(revision, comp)).copied()) {
        return k;
    }
    let k = disk_cache::comp_content_key(project, ItemId(comp));
    if let Ok(mut m) = keys.lock() {
        if m.len() > 256 {
            m.clear();
        }
        m.insert((revision, comp), k);
    }
    k
}

/// The identity of a comp's pixels in `project`, cheap enough to take at every revision: the
/// addresses of the comps it draws (an edit replaces a comp's `Arc`: `Arc::make_mut` copies it
/// while another snapshot holds it, and [`Frames::content_of`] keeps one for every identity it
/// hands out), and by value the project settings and the footage, solids and proxies it uses.
/// With expressions in its comps (which can read anything) every item counts.
pub fn comp_content(project: &Project, comp: ItemId) -> u64 {
    comp_content_with(project, comp, |_, c| Arc::as_ptr(c))
}

/// [`comp_content`] with each comp's address given by `addr`.
fn comp_content_with(project: &Project, comp: ItemId, addr: impl Fn(ItemId, &Arc<Comp>) -> *const Comp) -> u64 {
    use std::hash::{Hash, Hasher};
    fn has_expression(g: &PropGroup) -> bool {
        g.children.iter().any(|c| match c {
            Node::Prop(p) => p.expr.is_some(),
            Node::Group(sub) => has_expression(sub),
        })
    }
    let mut h = std::collections::hash_map::DefaultHasher::new();
    hash_debug(&mut h, &project.settings);
    let mut seen = BTreeSet::new();
    let mut stack = vec![comp];
    let mut expressions = false;
    while let Some(id) = stack.pop() {
        if !seen.insert(id) {
            continue;
        }
        let Some(ItemKind::Comp(c)) = project.item(id).map(|i| &i.kind) else { continue };
        for l in &c.layers {
            if let LayerSource::Comp { item } | LayerSource::Footage { item } | LayerSource::Solid { item } = l.source {
                stack.push(item);
            }
            expressions = expressions || has_expression(&l.props);
        }
    }
    let ids: Vec<ItemId> = if expressions { project.items.keys().copied().collect() } else { seen.into_iter().collect() };
    for id in ids {
        let Some(it) = project.item(id) else { continue };
        id.0.hash(&mut h);
        if expressions {
            // (`comp("Name")`, `footage("Name")`)
            it.name.hash(&mut h);
        }
        match &it.kind {
            ItemKind::Comp(c) => (addr(id, c) as usize).hash(&mut h),
            k => hash_debug(&mut h, k),
        }
        hash_debug(&mut h, &it.proxy);
    }
    comp.0.hash(&mut h);
    h.finish()
}

/// Hash `v`'s `Debug` text and a terminator, streamed (no intermediate `String`: footage of a
/// long image sequence formats every frame's path).
fn hash_debug(h: &mut impl std::hash::Hasher, v: &impl std::fmt::Debug) {
    struct W<'a, H: std::hash::Hasher>(&'a mut H);
    impl<H: std::hash::Hasher> std::fmt::Write for W<'_, H> {
        fn write_str(&mut self, s: &str) -> std::fmt::Result {
            self.0.write(s.as_bytes());
            Ok(())
        }
    }
    use std::fmt::Write;
    let _ = write!(W(h), "{v:?}");
    h.write_u8(0xff);
}

/// Hash of render options (part of [`FrameKey`] and of the disk key). The scale counts as the
/// key's thousandths ([`FrameKey::scale`]): a series rebuilt from the key (the cache bars) has
/// the same options as the frames rendered at 1/3 = 0.333… (#595).
pub fn opts_hash(o: &RenderOpts) -> u64 {
    let o = RenderOpts { scale: (o.scale * 1000.0).round() / 1000.0, ..*o };
    let mut h = disk_cache::Hash128::default();
    h.write(format!("{o:?}").as_bytes());
    h.finish() as u64
}

/// Disk key of a viewer frame (render options included: region of interest, draft…).
fn frame_disk_key(keys: &ContentKeys, project: &Project, key: &FrameKey, opts_hash: u64) -> u128 {
    let content = content_key(keys, project, key.revision, key.comp);
    disk_cache::frame_key(content, key.frame, key.scale, key.view ^ opts_hash)
}

/// A frame for a [`RemoteFrames`] renderer.
pub struct RemoteJob {
    pub project: Arc<Project>,
    pub revision: u64,
    pub comp: ItemId,
    pub t: Tick,
    pub opts: RenderOpts,
    /// The frame's disk-cache key, when the renderer keeps a disk cache
    /// ([`RemoteFrames::disk`]): served from it on a hit, stored there after a render.
    pub disk_key: Option<u128>,
}

/// Premultiplied RGBA8 pixels from a [`RemoteFrames`] renderer.
pub struct RemoteFrame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    /// Render time (ms).
    pub ms: f64,
}

/// Called once with a remote frame (or why there is none).
pub type RemoteDone = Box<dyn FnOnce(Result<RemoteFrame, String>) + Send>;

/// Renders viewer frames elsewhere (the browser's frame worker), so the UI thread never does.
pub trait RemoteFrames: Send + Sync {
    /// Frames it can render at the same time (0 when it is not usable: frames then render on
    /// the UI thread again).
    fn slots(&self) -> usize;
    /// Start rendering `job`; `done` runs when the frame arrives.
    fn start(&self, job: RemoteJob, done: RemoteDone);
    /// Drop cached intermediate results (Edit ▸ Purge).
    fn purge(&self) {}
    /// It renders GPU effects (its own GPU device): frames with effects go to it rather than
    /// the UI thread's GPU compositor, which would run their effects on the CPU.
    fn gpu(&self) -> bool {
        false
    }
    /// It keeps a disk cache of frames (the browser's Origin Private File System).
    fn disk(&self) -> bool {
        false
    }
    /// The disk cache holds the frame of this key (the blue cache bar).
    fn disk_contains(&self, _key: u128) -> bool {
        false
    }
}

/// Whether a comp (or a comp it nests) has a layer with enabled effects.
pub fn comp_has_effects(project: &Project, comp: ItemId) -> bool {
    fn walk(p: &Project, comp: ItemId, depth: usize) -> bool {
        let Some(c) = p.comp(comp) else { return false };
        c.layers.iter().any(|l| {
            l.switches.effects && l.effects().is_some_and(|fx| fx.groups().any(|g| g.enabled))
                || depth < 16 && matches!(l.source, effectcraft_engine::project::LayerSource::Comp { item } if walk(p, item, depth + 1))
        })
    }
    walk(project, comp, 0)
}

/// See [`Frames::failed`]: (key, revision) pairs.
type Failed = Arc<Mutex<HashSet<(FrameKey, u64)>>>;

/// Failed frames remembered at most (then forgotten, so retried once).
const MAX_FAILED: usize = 256;

#[cfg(not(target_arch = "wasm32"))]
struct BackgroundPermit {
    busy: Arc<AtomicBool>,
    ctx: Option<egui::Context>,
}

#[cfg(not(target_arch = "wasm32"))]
impl Drop for BackgroundPermit {
    fn drop(&mut self) {
        self.busy.store(false, Ordering::Release);
        if let Some(ctx) = &self.ctx {
            ctx.request_repaint();
        }
    }
}

pub struct Frames {
    cache: Arc<Mutex<Cache>>,
    /// Keys queued or rendering.
    inflight: Arc<Mutex<HashSet<FrameKey>>>,
    /// Frames whose render panicked, with the project revision they were asked for at: not asked
    /// for again until the project changes (or the caches are purged), instead of re-rendering
    /// (and panicking) at every UI frame.
    failed: Failed,
    /// Pending jobs, picked by priority when a pool thread frees up (the viewer's frame first).
    queue: Arc<Mutex<Queue>>,
    #[cfg(not(target_arch = "wasm32"))]
    /// None if the OS refused the threads: jobs then go to rayon's global pool.
    pool: Option<Arc<rayon::ThreadPool>>,
    #[cfg(not(target_arch = "wasm32"))]
    background_running: Arc<AtomicBool>,
    ctx: Option<egui::Context>,
    /// Render time of the last viewer (urgent) frame in ms, for the Info panel / perf readout.
    pub last_ms: Arc<Mutex<f64>>,
    playback_budget: Arc<Mutex<crate::playback_budget::Budget>>,
    content_keys: ContentKeys,
    gpu_retired: Arc<AtomicBool>,
    /// [`comp_content`] identities by (revision, comp), with the project that keeps an identity
    /// found by [`Frames::identity`] in another project.
    identities: Mutex<HashMap<(u64, u64), Identity>>,
    /// The last new identity of each comp and its project (see [`Frames::identity`]).
    bases: Mutex<HashMap<u64, (u64, Arc<Project>)>>,
    /// A project snapshot for each identity handed out, with when it was last asked for (see
    /// [`Frames::content_of`]); the counter.
    keepers: Mutex<(u64, HashMap<u64, (u64, Arc<Project>)>)>,
    /// Where frames render instead of the UI thread (wasm32), and how many it is rendering.
    remote: Option<Arc<dyn RemoteFrames>>,
    remote_busy: Arc<Mutex<usize>>,
    /// The cache bar's frames, remembered while nothing they depend on changes (the bar is drawn
    /// every UI frame).
    bar: Mutex<CacheBar>,
}

/// See [`Frames::cached_frames`] and [`Frames::disk_frames`]: (series with frame 0, what else
/// the answer depends on, the answer), most recent last.
#[derive(Default)]
struct CacheBar {
    ram: Vec<(FrameKey, u64, Vec<i64>)>,
    disk: Vec<(FrameKey, [u64; 5], Vec<i64>)>,
}

/// Series remembered by [`CacheBar`] (the Timeline, Preview panel and playback ask for one or two).
const BAR_SERIES: usize = 4;

fn remember<K: Copy>(v: &mut Vec<(FrameKey, K, Vec<i64>)>, series: FrameKey, k: K, frames: &[i64]) {
    if v.len() >= BAR_SERIES {
        v.remove(0);
    }
    v.push((series, k, frames.to_vec()));
}

impl Default for Frames {
    fn default() -> Self {
        #[cfg(not(target_arch = "wasm32"))]
        let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).clamp(2, 16);
        Frames {
            cache: Arc::new(Mutex::new(Cache { map: HashMap::new(), order: VecDeque::new(), protected: HashMap::new(), bytes: 0, gpu_bytes: 0, budget: 3 << 30, gpu_budget: GPU_BUDGET, generation: 0 })),
            inflight: Arc::new(Mutex::new(HashSet::new())),
            failed: Failed::default(),
            queue: Arc::new(Mutex::new(Queue::default())),
            #[cfg(not(target_arch = "wasm32"))]
            // A panicking job would abort the whole process (rayon's default for `spawn`): log it
            // and lose only that frame (the panic hook has logged the message).
            pool: rayon::ThreadPoolBuilder::new()
                .num_threads(threads)
                .thread_name(|i| format!("ec-frame-{i}"))
                .panic_handler(|_| log::error!("a frame render panicked; that frame was skipped"))
                .build()
                .ok()
                .map(Arc::new),
            #[cfg(not(target_arch = "wasm32"))]
            background_running: Arc::default(),
            ctx: None,
            last_ms: Arc::new(Mutex::new(0.0)),
            playback_budget: Arc::default(),
            content_keys: Arc::default(),
            gpu_retired: Arc::default(),
            identities: Mutex::default(),
            bases: Mutex::default(),
            keepers: Mutex::default(),
            remote: None,
            remote_busy: Arc::default(),
            bar: Mutex::default(),
        }
    }
}

/// Frames that may render at once given `job`'s size: each takes about four `f32` frames (the
/// comp canvas, layer buffers, matte / adjustment temporaries), and together they stay within
/// the layer cache's memory budget (Memory & CPU settings): a 4K comp (~530 MB a frame) renders
/// two frames at once on the default 1 GB, an HD comp eight. At least one.
fn max_running(job: &Job) -> usize {
    let (w, h) = job.src.project.comp(job.comp).map_or((0, 0), |c| (c.width, c.height));
    let s = job.opts.scale.clamp(0.01, 4.0);
    let frame = ((w as f64 * s).ceil() * (h as f64 * s).ceil() * 16.0 * 4.0).max(1.0);
    ((job.src.layer_cache.budget() as f64 / frame) as usize).max(1)
}

/// Premultiplied f32 → egui premultiplied Color32 (alpha kept, so the viewer can show the
/// transparency grid or the comp background colour underneath).
pub fn to_color_image(img: &effectcraft_engine::render::Image) -> egui::ColorImage {
    let px: Vec<egui::Color32> = img
        .data
        .par_iter()
        .map(|p| {
            let a = p[3].clamp(0.0, 1.0);
            let c = |v: f32| (v.clamp(0.0, a) * 255.0 + 0.5) as u8;
            egui::Color32::from_rgba_premultiplied(c(p[0]), c(p[1]), c(p[2]), (a * 255.0 + 0.5) as u8)
        })
        .collect();
    egui::ColorImage::new([img.width as usize, img.height as usize], px)
}

/// The whole factor an image of `size` is averaged down by for a texture: at least `min_k`, and
/// the smallest that fits its sides in `max_side` (frames of comps wider than the GPU's texture
/// limit, #201).
pub fn fit_factor(size: [usize; 2], max_side: usize, min_k: usize) -> usize {
    size[0].max(size[1]).div_ceil(max_side.max(1)).max(min_k).max(1)
}

/// The size `size` is shown at in a texture whose sides can't exceed `max_side`.
pub fn fitted_size(size: [usize; 2], max_side: usize) -> [usize; 2] {
    let k = fit_factor(size, max_side, 1);
    [size[0].div_ceil(k), size[1].div_ceil(k)]
}

/// The texture coordinates of the image within a texture averaged down by `k`: its last texels
/// cover fewer pixels than the factor, so drawn over the image's area with these the texels sit
/// exactly where their pixels are.
pub fn fitted_uv(size: [usize; 2], k: usize) -> egui::Rect {
    let k = k.max(1);
    let f = |n: usize| (n as f64 / (n.div_ceil(k).max(1) * k) as f64) as f32;
    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(f(size[0]), f(size[1])))
}

/// `img` as a texture the renderer accepts: unchanged when it fits in `max_side`, else averaged
/// down by a whole factor (premultiplied pixels average correctly). The viewer draws it over the
/// same area, so only detail beyond the GPU's texture limit is lost, instead of the upload
/// failing (#201).
pub fn fit_texture(img: egui::ColorImage, max_side: usize) -> egui::ColorImage {
    fit_texture_by(img, max_side, 1)
}

/// [`fit_texture`] averaging down by at least `min_k` (see [`fit_factor`]).
pub fn fit_texture_by(img: egui::ColorImage, max_side: usize, min_k: usize) -> egui::ColorImage {
    let k = fit_factor(img.size, max_side, min_k);
    if k == 1 {
        return img;
    }
    average_down(&img, k)
}

/// [`fit_texture_by`] for a shared frame: one that fits is handed to egui as it is (no copy of
/// its pixels; egui uploads straight from the `Arc`).
pub fn fit_texture_shared(img: &Arc<egui::ColorImage>, max_side: usize, min_k: usize) -> Arc<egui::ColorImage> {
    let k = fit_factor(img.size, max_side, min_k);
    if k == 1 {
        return Arc::clone(img);
    }
    Arc::new(average_down(img, k))
}

/// `img` averaged down by `k` (rounded up at the edges).
fn average_down(img: &egui::ColorImage, k: usize) -> egui::ColorImage {
    let [w, h] = img.size;
    let (fw, fh) = (w.div_ceil(k), h.div_ceil(k));
    let px: Vec<egui::Color32> = (0..fw * fh)
        .into_par_iter()
        .map(|i| {
            let (x0, y0) = ((i % fw) * k, (i / fw) * k);
            let (mut sum, mut n) = ([0u64; 4], 0u64);
            for y in y0..(y0 + k).min(h) {
                for c in img.pixels.get(y * w + x0..y * w + (x0 + k).min(w)).unwrap_or_default() {
                    for (s, v) in sum.iter_mut().zip(c.to_array()) {
                        *s += v as u64;
                    }
                    n += 1;
                }
            }
            let n = n.max(1);
            let [r, g, b, a] = sum.map(|s| ((s + n / 2) / n) as u8);
            egui::Color32::from_rgba_premultiplied(r, g, b, a)
        })
        .collect();
    egui::ColorImage::new([fw, fh], px)
}

/// The largest texture side the renderer accepts.
pub fn max_texture_side(ctx: &egui::Context) -> usize {
    ctx.input(|i| i.max_texture_side)
}

/// `ctx.load_texture` for frame-sized images: fitted to the renderer's texture limit first.
pub fn load_fitted(ctx: &egui::Context, name: impl Into<String>, img: egui::ColorImage, opts: egui::TextureOptions) -> egui::TextureHandle {
    ctx.load_texture(name, fit_texture(img, max_texture_side(ctx)), opts)
}

impl Frames {
    #[cfg(test)]
    pub(crate) fn playback_sample(&self, key: FrameKey, ms: f64) {
        let mut budget = self.playback_budget.lock().unwrap();
        if let Some(ticket) = budget.ticket(key) {
            budget.record(ticket, ms, false);
        }
    }

    pub(crate) fn adapt_playback(&self, config: Option<crate::playback_budget::Config>) -> bool {
        self.playback_budget.lock().unwrap_or_else(std::sync::PoisonError::into_inner).configure(config)
    }

    /// Retire only GPU preview state; documents, CPU frames and running job leases survive.
    /// Publication checks the same latch while holding the cache lock, so a late GPU frame
    /// cannot restore an invalid texture after this removal.
    pub fn retire_gpu(&self) {
        self.gpu_retired.store(true, Ordering::Release);
        self.adapt_playback(None);
        if let Ok(mut cache) = self.cache.lock() {
            let keys: Vec<_> = cache.map.iter().filter_map(|(key, image)| image.is_gpu().then_some(*key)).collect();
            for key in keys {
                cache.remove(&key);
            }
            let Cache { map, order, .. } = &mut *cache;
            order.retain(|key| map.contains_key(key));
        }
        if let Some(ctx) = &self.ctx {
            ctx.request_repaint();
        }
    }

    /// One auxiliary render across all layers, admitted before Rayon can nest it inside
    /// another render's parallel pixel loop (#586/#647). Rejected callers retry on repaint.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn try_spawn_background(&self, task: impl FnOnce() + Send + 'static) -> bool {
        if self.background_running.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire).is_err() {
            return false;
        }
        let permit = BackgroundPermit { busy: self.background_running.clone(), ctx: self.ctx.clone() };
        let run = move || {
            let _permit = permit;
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(task)).is_err() {
                log::error!("an auxiliary preview render panicked; its slot was released");
            }
        };
        match &self.pool {
            Some(pool) => pool.spawn(run),
            None => rayon::spawn(run),
        }
        true
    }

    /// Actual native frame-worker capacity, including the OS fallback pool.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn parallelism(&self) -> usize {
        self.pool.as_ref().map_or_else(rayon::current_num_threads, |p| p.current_num_threads())
    }
    #[cfg(target_arch = "wasm32")]
    pub fn parallelism(&self) -> usize {
        self.remote_slots().max(1)
    }
    pub fn set_context(&mut self, ctx: &egui::Context) {
        if self.ctx.is_none() {
            self.ctx = Some(ctx.clone());
        }
    }

    /// The egui context (to request repaints from callbacks).
    pub fn context(&self) -> Option<egui::Context> {
        self.ctx.clone()
    }

    /// The cached frame, which becomes the most recently used.
    pub fn get(&self, k: &FrameKey) -> Option<FrameImage> {
        let mut c = self.cache.lock().ok()?;
        let img = c.map.get(k).cloned()?;
        c.touch(k);
        Some(img)
    }

    /// Prefer the exact requested frame. While an edit renders, advance to the newest completed
    /// revision of the same comp, time, scale, view and render options instead of freezing on the
    /// image from before the drag. Input may arrive before the viewer draws on every UI frame,
    /// so even a fast worker cannot always finish the *latest* revision in time.
    ///
    /// Never move backwards when workers finish out of order, and never substitute another time
    /// or view while scrubbing or switching comps. The exact content lookup above also means an
    /// undo can immediately reuse an old cache entry, regardless of its original revision.
    pub fn get_for_viewer(&self, wanted: &FrameKey, shown: Option<&FrameKey>) -> Option<(FrameKey, FrameImage)> {
        if let Some(img) = self.get(wanted) {
            return Some((*wanted, img));
        }
        let shown = shown?;
        if shown.comp != wanted.comp || shown.frame != wanted.frame || shown.view != wanted.view || shown.revision >= wanted.revision {
            return None;
        }
        let mut c = self.cache.lock().ok()?;
        let newest = c
            .map
            .keys()
            .filter(|k| {
                k.revision > shown.revision
                    && k.revision < wanted.revision
                    && k.comp == wanted.comp
                    && k.frame == wanted.frame
                    && k.scale == wanted.scale
                    && k.view == wanted.view
                    && k.opts == wanted.opts
            })
            .max_by_key(|k| k.revision)
            .copied()?;
        let img = c.map.get(&newest).cloned()?;
        c.touch(&newest);
        Some((newest, img))
    }

    /// The [`comp_content`] identity of `comp` in `project` at `revision` (taken once per
    /// revision). The project is kept while frames of that identity are cached or rendering, so
    /// the comp addresses it hashes can't be reused by other comps meanwhile.
    pub fn content_of(&self, project: &Arc<Project>, revision: u64, comp: ItemId) -> u64 {
        let known = self.identities.lock().ok().and_then(|m| m.get(&(revision, comp.0)).cloned());
        let (id, base) = known.unwrap_or_else(|| {
            let found = self.identity(project, comp);
            if let Ok(mut m) = self.identities.lock() {
                if m.len() > 256 {
                    m.clear();
                }
                m.insert((revision, comp.0), found.clone());
            }
            found
        });
        if let Ok(mut guard) = self.keepers.lock() {
            let (clock, k) = &mut *guard;
            *clock += 1;
            let now = *clock;
            k.entry(id).or_insert_with(|| (now, base.unwrap_or_else(|| project.clone()))).0 = now;
            if k.len() > MAX_KEEPERS {
                let inflight: HashSet<u64> = self.inflight.lock().map(|s| s.iter().map(|f| f.content).collect()).unwrap_or_default();
                if let Ok(mut c) = self.cache.lock() {
                    // Snapshots of identities without frames go; past the limit the least
                    // recently asked for go too, with their frames (a long drag leaves many).
                    let cached: HashSet<u64> = c.map.keys().map(|f| f.content).collect();
                    k.retain(|i, _| *i == id || cached.contains(i) || inflight.contains(i));
                    if k.len() > MAX_KEEPERS {
                        let mut ages: Vec<(u64, u64)> = k.iter().filter(|(i, _)| **i != id && !inflight.contains(i)).map(|(i, (t, _))| (*t, *i)).collect();
                        ages.sort_unstable();
                        let gone: HashSet<u64> = ages.into_iter().take(k.len().saturating_sub(MAX_KEEPERS / 2)).map(|(_, i)| i).collect();
                        k.retain(|i, _| !gone.contains(i));
                        c.drop_contents(&gone);
                    }
                }
            }
        }
        id
    }

    /// The [`comp_content`] identity of `comp` in `project`, or the comp's last new identity when
    /// every comp it draws is the same or draws the same pixels there ([`Comp::same_pixels`]:
    /// only Audio, Lock or Shy switches changed), so toggling those keeps the cached frames
    /// (#103). That identity hashes the addresses of the comps in its project, which is
    /// returned to keep them.
    fn identity(&self, project: &Arc<Project>, comp: ItemId) -> Identity {
        let plain = comp_content(project, comp);
        let Ok(mut bases) = self.bases.lock() else { return (plain, None) };
        if let Some((base_id, base)) = bases.get(&comp.0)
            && *base_id != plain
            && comp_content_with(project, comp, |id, c| match base.item(id).map(|i| &i.kind) {
                Some(ItemKind::Comp(b)) if Arc::ptr_eq(b, c) || b.same_pixels(c) => Arc::as_ptr(b),
                _ => Arc::as_ptr(c),
            }) == *base_id
        {
            return (*base_id, Some(base.clone()));
        }
        if bases.len() > MAX_KEEPERS {
            bases.clear();
        }
        bases.insert(comp.0, (plain, project.clone()));
        (plain, None)
    }

    pub fn is_cached(&self, k: &FrameKey) -> bool {
        self.cache.lock().map(|c| c.map.contains_key(k)).unwrap_or(false)
    }

    pub fn inflight(&self) -> usize {
        self.inflight.lock().map(|s| s.len()).unwrap_or(0)
    }

    /// Cached frame numbers of the series of `base` (its comp content, scale, view and render
    /// options) — for the cache bar.
    pub fn cached_frames(&self, base: &FrameKey) -> Vec<i64> {
        let series = FrameKey { frame: 0, ..*base };
        let Ok(c) = self.cache.lock() else { return vec![] };
        let mut bar = self.bar.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((_, _, v)) = bar.ram.iter().find(|(s, g, _)| *s == series && *g == c.generation) {
            return v.clone();
        }
        let mut v: Vec<i64> = c.map.keys().filter(|k| k.same_series(base)).map(|k| k.frame).collect();
        v.sort_unstable();
        bar.ram.retain(|(s, ..)| *s != series);
        remember(&mut bar.ram, series, c.generation, &v);
        v
    }

    /// Frames of the series of `base` in the disk cache but not in RAM — the blue cache bar.
    /// `opts` are the viewer's render options (part of the disk key).
    pub fn disk_frames(&self, src: &RenderSource, base: &FrameKey, frames: i64, opts: &RenderOpts) -> Vec<i64> {
        let remote = self.remote.as_ref().filter(|r| r.disk());
        let contains = |k: u128| match (&src.disk, remote) {
            (Some(dc), _) => dc.contains(disk_cache::Kind::Frame, k),
            (None, Some(r)) => r.disk_contains(k),
            (None, None) => false,
        };
        if src.disk.is_none() && remote.is_none() {
            return vec![];
        }
        let oh = opts_hash(opts);
        // Remembered until the frames in RAM or on disk, the project or the series change (the
        // remote disk cache gets its frames with the RAM cache's).
        let series = FrameKey { frame: 0, ..*base };
        let ram_generation = self.cache.lock().map(|c| c.generation).unwrap_or(0);
        let k = [base.revision, frames as u64, oh, ram_generation, src.disk.as_ref().map_or(0, |dc| dc.generation())];
        if let Ok(bar) = self.bar.lock()
            && let Some((_, _, v)) = bar.disk.iter().find(|(s, g, _)| *s == series && *g == k)
        {
            return v.clone();
        }
        let ram: HashSet<i64> = self.cached_frames(base).into_iter().collect();
        let v: Vec<i64> = (0..frames.clamp(0, 100_000))
            .filter(|f| !ram.contains(f))
            .filter(|f| contains(frame_disk_key(&self.content_keys, &src.project, &FrameKey { frame: *f, ..*base }, oh)))
            .collect();
        if let Ok(mut bar) = self.bar.lock() {
            bar.disk.retain(|(s, ..)| *s != series);
            remember(&mut bar.disk, series, k, &v);
        }
        v
    }

    /// Change the RAM preview cache budget (Settings ▸ Memory & CPU), evicting the oldest
    /// frames when it shrinks.
    pub fn set_budget(&self, bytes: usize) {
        if let Ok(mut c) = self.cache.lock() {
            c.budget = bytes;
            c.evict_to_budget();
        }
    }

    /// Keep the playhead and its immediate look-ahead ahead of speculative frames for eviction.
    /// This never overrides either memory budget, even if the whole window does not fit.
    pub fn protect_frames(&self, base: FrameKey, frames: impl IntoIterator<Item = i64>) {
        if let Ok(mut cache) = self.cache.lock() {
            cache.protected.clear();
            cache.protected.extend(frames.into_iter().enumerate().map(|(rank, frame)| (FrameKey { frame, ..base }, rank)));
        }
    }

    pub fn budget(&self) -> usize {
        self.cache.lock().map(|c| c.budget).unwrap_or(0)
    }

    pub fn clear(&self) {
        self.adapt_playback(None);
        if let Some(r) = &self.remote {
            r.purge();
        }
        if let Ok(mut c) = self.cache.lock() {
            c.map.clear();
            c.generation = c.generation.wrapping_add(1);
            c.order.clear();
            c.bytes = 0;
            c.gpu_bytes = 0;
        }
        if let Ok(mut f) = self.failed.lock() {
            f.clear();
        }
        // Disk keys hash the footage files' size and time, but are remembered per revision; a
        // file reloaded from disk without a new revision (a re-rendered image sequence) must be
        // keyed again, or the disk cache serves the frames from before.
        if let Ok(mut k) = self.content_keys.lock() {
            k.clear();
        }
    }

    /// The render of `key` panicked at its revision (it is not asked for again until then).
    pub fn failed(&self, key: &FrameKey) -> bool {
        self.failed.lock().is_ok_and(|f| f.contains(&(*key, key.revision)))
    }

    /// Queue a prefetch frame (no-op if cached or already queued/rendering).
    pub fn request(&self, src: &RenderSource, key: FrameKey, comp: ItemId, t: Tick, opts: RenderOpts) {
        self.request_with(src, key, comp, t, opts, false);
    }

    /// Queue the frame the viewer is showing: it jumps ahead of every prefetch job.
    pub fn request_urgent(&self, src: &RenderSource, key: FrameKey, comp: ItemId, t: Tick, opts: RenderOpts) {
        self.request_with(src, key, comp, t, opts, true);
    }

    /// An urgent (viewer) frame is queued or rendering.
    pub fn urgent_pending(&self) -> bool {
        self.queue.lock().map(|q| q.urgent_running > 0 || q.jobs.iter().any(|j| j.urgent)).unwrap_or(false)
    }

    fn request_with(&self, src: &RenderSource, key: FrameKey, comp: ItemId, t: Tick, opts: RenderOpts, urgent: bool) {
        if self.is_cached(&key) || self.failed(&key) {
            return;
        }
        let Ok(mut q) = self.queue.lock() else { return };
        let Ok(mut inf) = self.inflight.lock() else { return };
        // Jobs for an older project revision can never be shown: drop them.
        let before = q.jobs.len();
        q.jobs.retain(|j| j.key.revision >= key.revision);
        if q.jobs.len() < before {
            inf.retain(|k| k.revision >= key.revision || q.running.contains(k));
        }
        if urgent {
            // Only the newest viewer frame is urgent. The comp's earlier viewer frames that
            // haven't started are dropped (scrubbing: rendering every frame the pointer passed
            // would hold the workers the frame under it needs); frames already rendering finish
            // into the cache. During edits the viewer advances through compatible completed
            // revisions; scrubbing holds its last frame until the requested time is in (#284).
            // Other comps' viewer frames become prefetch.
            q.jobs.retain(|j| {
                let superseded = j.urgent && j.key != key && j.key.comp == key.comp;
                if superseded {
                    inf.remove(&j.key);
                }
                !superseded
            });
            for j in q.jobs.iter_mut() {
                j.urgent = j.key == key;
            }
        }
        if !inf.insert(key) {
            return;
        }
        q.seq += 1;
        let seq = q.seq;
        q.jobs.push(Job { key, src: src.clone(), comp, t, opts, urgent, seq });
        drop(inf);
        drop(q);
        #[cfg(not(target_arch = "wasm32"))]
        if self.remote.is_none() {
            // One spawn per job; each spawn renders whichever queued job matters most right now.
            self.worker().spawn_next();
        }
        #[cfg(target_arch = "wasm32")]
        if let Some(ctx) = &self.ctx {
            // rendered by `pump` after this UI frame
            ctx.request_repaint();
        }
    }

    fn worker(&self) -> Worker {
        Worker {
            queue: self.queue.clone(),
            cache: self.cache.clone(),
            inflight: self.inflight.clone(),
            failed: self.failed.clone(),
            ctx: self.ctx.clone(),
            last: self.last_ms.clone(),
            playback_budget: self.playback_budget.clone(),
            content_keys: self.content_keys.clone(),
            gpu_retired: self.gpu_retired.clone(),
            #[cfg(not(target_arch = "wasm32"))]
            pool: self.pool.clone(),
        }
    }

    /// Render frames with `remote` from now on (`None`: on this thread again).
    pub fn set_remote(&mut self, remote: Option<Arc<dyn RemoteFrames>>) {
        self.remote = remote;
    }

    /// The remote renderer keeps a disk cache (the browser's).
    pub fn remote_disk(&self) -> bool {
        self.remote.as_ref().is_some_and(|r| r.disk())
    }

    /// Whether frames go to a usable [`RemoteFrames`].
    pub fn remote_active(&self) -> bool {
        self.remote.as_ref().is_some_and(|r| r.slots() > 0)
    }

    /// Frames the remote renderer renders at once (0 without one).
    pub fn remote_slots(&self) -> usize {
        self.remote.as_ref().map(|r| r.slots()).unwrap_or(0)
    }

    /// Frames being rendered remotely.
    pub fn remote_busy(&self) -> usize {
        self.remote_busy.lock().map(|b| *b).unwrap_or(0)
    }

    /// Hand queued frames to the remote renderer while it has free slots, most important first
    /// (the viewer's frame, then prefetch in request order; a viewer frame requested while every
    /// slot is busy goes to the first one that frees up). Frames that the GPU compositor can keep
    /// on the GPU render here instead (no readback). Returns how many were started.
    pub fn dispatch_remote(&self) -> usize {
        let Some(remote) = self.remote.clone() else { return 0 };
        let slots = remote.slots();
        let w = self.worker();
        let mut started = 0;
        loop {
            let busy = self.remote_busy();
            if busy >= slots {
                break;
            }
            let job = {
                let Ok(mut q) = self.queue.lock() else { break };
                let Some(i) = Queue::best(&q.jobs) else { break };
                let job = q.jobs.swap_remove(i);
                q.running.insert(job.key);
                if job.urgent {
                    q.urgent_running += 1;
                }
                job
            };
            // The GPU compositor here keeps the frame on the GPU, but runs layer effects on the
            // CPU (this thread can't wait for readbacks): frames with effects go to workers
            // that render them on their own GPU.
            let gpu_here = job.src.gpu.is_some() && job.src.gpu_display && !(remote.gpu() && comp_has_effects(&job.src.project, job.comp));
            if gpu_here {
                let t0 = web_time::Instant::now();
                if let Some(img) = w.render_gpu(&job) {
                    w.finish(&job, img, t0);
                    continue;
                }
            }
            if let Ok(mut b) = self.remote_busy.lock() {
                *b += 1;
            }
            started += 1;
            let (key, urgent) = (job.key, job.urgent);
            let fw = self.worker();
            let busy = self.remote_busy.clone();
            let t0 = web_time::Instant::now();
            let disk_key = remote.disk().then(|| frame_disk_key(&w.content_keys, &job.src.project, &key, opts_hash(&job.opts)));
            let rj = RemoteJob { project: job.src.project.clone(), revision: key.revision, comp: job.comp, t: job.t, opts: job.opts, disk_key };
            remote.start(
                rj,
                Box::new(move |r: Result<RemoteFrame, String>| {
                    if let Ok(mut b) = busy.lock() {
                        *b = b.saturating_sub(1);
                    }
                    match r {
                        Ok(f) => {
                            let img = egui::ColorImage::from_rgba_premultiplied([f.width as usize, f.height as usize], &f.rgba);
                            fw.finish_key(key, urgent, FrameImage::Cpu(Arc::new(img)), t0);
                        }
                        Err(e) => {
                            // Not rendered (a stale revision, a lost worker): the viewer asks again.
                            log::debug!("remote frame: {e}");
                            fw.release(key, urgent);
                        }
                    }
                }),
            );
        }
        started
    }

    /// Render queued frames, most important first. With a usable [`RemoteFrames`] they are only
    /// handed over ([`Frames::dispatch_remote`]); otherwise they render on this thread until the
    /// queue is empty or `budget` has passed (at least one frame). Returns whether frames are
    /// still queued or rendering remotely. Only wasm32 needs this (native frames render on the
    /// pool); elsewhere it does nothing.
    pub fn pump(&self, budget: std::time::Duration) -> bool {
        if cfg!(not(target_arch = "wasm32")) {
            return false;
        }
        if self.remote_active() {
            self.dispatch_remote();
            return self.queue.lock().map(|q| !q.jobs.is_empty()).unwrap_or(false) || self.remote_busy() > 0;
        }
        let t0 = web_time::Instant::now();
        let w = self.worker();
        while w.run_next() {
            if t0.elapsed() >= budget {
                break;
            }
        }
        self.queue.lock().map(|q| !q.jobs.is_empty()).unwrap_or(false)
    }
}

/// Frames that rendered faster than this aren't written to the disk cache: rendering them
/// again costs about as much as reading and decompressing them, and the copy + LZ4 compression
/// would come out of the frame worker's time.
const DISK_MIN_RENDER: std::time::Duration = std::time::Duration::from_millis(25);

/// Whether a freshly rendered frame goes to the disk cache: only frames that were expensive to
/// render, and no prefetch frame while more prefetch is queued (playback and RAM preview keep the
/// workers busy; there compression would slow the frames being waited for). The viewer's frame
/// is kept whenever it was expensive.
fn keep_on_disk(render: std::time::Duration, urgent: bool, prefetch_queued: usize) -> bool {
    render >= DISK_MIN_RENDER && (urgent || prefetch_queued == 0)
}

/// Renders queued jobs (on a pool thread, or the UI thread on wasm32).
#[derive(Clone)]
struct Worker {
    queue: Arc<Mutex<Queue>>,
    cache: Arc<Mutex<Cache>>,
    inflight: Arc<Mutex<HashSet<FrameKey>>>,
    failed: Failed,
    ctx: Option<egui::Context>,
    last: Arc<Mutex<f64>>,
    playback_budget: Arc<Mutex<crate::playback_budget::Budget>>,
    content_keys: ContentKeys,
    gpu_retired: Arc<AtomicBool>,
    /// The pool frames render on (`None`: rayon's global pool).
    #[cfg(not(target_arch = "wasm32"))]
    pool: Option<Arc<rayon::ThreadPool>>,
}

impl Worker {
    /// Run [`Worker::run_next`] on the frame pool.
    #[cfg(not(target_arch = "wasm32"))]
    fn spawn_next(&self) {
        let w = self.clone();
        let job = move || {
            w.run_next();
        };
        match &self.pool {
            Some(pool) => pool.spawn(job),
            None => rayon::spawn(job),
        }
    }

    /// Render the queued job that matters most right now. `false` when the queue was empty, or
    /// the job has to wait for memory (a finishing frame starts it, see [`Worker::release`]).
    fn run_next(&self) -> bool {
        let Worker { queue, content_keys, .. } = self;
        let job = {
            let Ok(mut q) = queue.lock() else { return false };
            let Some(i) = Queue::best(&q.jobs) else { return false };
            let j = &q.jobs[i];
            // Keep one viewer slot beyond the prefetch budget, but never nest another viewer
            // render inside it. Rayon can run queued jobs inside a parallel render: unlimited
            // urgent jobs then retain every obsolete frame's buffers during a drag (#647).
            // Wait in the queue, never on a pool thread needed by an unfinished render.
            if !cfg!(target_arch = "wasm32") && !q.can_start(j) {
                q.parked += 1;
                return false;
            }
            let job = q.jobs.swap_remove(i);
            q.running.insert(job.key);
            if job.urgent {
                q.urgent_running += 1;
            }
            job
        };
        let t0 = web_time::Instant::now();
        let disk = job.src.disk.as_ref().map(|dc| (dc.clone(), frame_disk_key(content_keys, &job.src.project, &job.key, opts_hash(&job.opts))));
        let from_disk = disk.as_ref().and_then(|(dc, k)| dc.get_frame(*k));
        let disk_hit = from_disk.is_some();
        let ticket = self.playback_budget.lock().unwrap_or_else(std::sync::PoisonError::into_inner).ticket(job.key);
        let service_start = web_time::Instant::now();
        let ci = match from_disk {
            Some(f) => FrameImage::Cpu(Arc::new(egui::ColorImage::from_rgba_premultiplied([f.width as usize, f.height as usize], &f.rgba))),
            None => match self.render_guarded(&job) {
                Some(ci) => ci,
                None => {
                    // Not "in progress" forever; not asked for again until the project changes.
                    self.give_up(job.key, job.urgent);
                    return true;
                }
            },
        };
        if !disk_hit
            && !(ci.is_gpu() && self.gpu_retired.load(Ordering::Acquire))
            && let Some(ticket) = ticket
        {
            self.playback_budget.lock().unwrap_or_else(std::sync::PoisonError::into_inner).record(
                ticket,
                service_start.elapsed().as_secs_f64() * 1000.0,
                ci.is_gpu(),
            );
        }
        let render_time = t0.elapsed();
        // Publish the shared frame and wake the viewer first; the disk cache's writer thread
        // compresses and writes the same snapshot.
        self.finish(&job, ci.clone(), t0);
        let backlog = queue.lock().map(|q| q.jobs.iter().filter(|j| !j.urgent).count()).unwrap_or(0);
        if !disk_hit
            && keep_on_disk(render_time, job.urgent, backlog)
            && let (Some((dc, k)), FrameImage::Cpu(img)) = (&disk, &ci)
        {
            // Hands over a reference to the frame (no copy, no compression here).
            dc.put_frame(*k, img.size[0] as u32, img.size[1] as u32, ColorBytes(img.clone()));
            if let Some(ctx) = &self.ctx {
                ctx.request_repaint();
            }
        }
        true
    }

    /// Store a finished frame and release its job.
    fn finish(&self, job: &Job, img: FrameImage, t0: web_time::Instant) {
        self.finish_key(job.key, job.urgent, img, t0);
    }

    fn finish_key(&self, key: FrameKey, urgent: bool, img: FrameImage, t0: web_time::Instant) {
        if urgent && let Ok(mut l) = self.last.lock() {
            *l = t0.elapsed().as_secs_f64() * 1000.0;
        }
        if let Ok(mut c) = self.cache.lock()
            && (!img.is_gpu() || !self.gpu_retired.load(Ordering::Acquire))
        {
            c.insert(key, img);
        }
        self.release(key, urgent);
    }

    /// The render of `key` failed: remember it (see [`Frames::failed`]) and release the job.
    fn give_up(&self, key: FrameKey, urgent: bool) {
        if let Ok(mut f) = self.failed.lock() {
            if f.len() >= MAX_FAILED {
                f.clear();
            }
            f.insert((key, key.revision));
        }
        self.release(key, urgent);
    }

    /// The job of `key` is no longer queued or running (rendered or dropped).
    fn release(&self, key: FrameKey, urgent: bool) {
        if let Ok(mut inf) = self.inflight.lock() {
            inf.remove(&key);
        }
        let mut restart = false;
        if let Ok(mut q) = self.queue.lock() {
            q.running.remove(&key);
            if urgent {
                q.urgent_running = q.urgent_running.saturating_sub(1);
            }
            // A job that waited for memory starts now (one per finished frame).
            if q.parked > 0 && !q.jobs.is_empty() {
                q.parked -= 1;
                restart = true;
            } else if q.jobs.is_empty() {
                q.parked = 0;
            }
        }
        #[cfg(not(target_arch = "wasm32"))]
        if restart {
            self.spawn_next();
        }
        #[cfg(target_arch = "wasm32")]
        let _ = restart;
        if let Some(ctx) = &self.ctx {
            ctx.request_repaint();
        }
    }

    /// [`Worker::render_job`], again on the CPU if it panicked (the panic hook has logged it);
    /// `None` if that panicked too.
    fn render_guarded(&self, job: &Job) -> Option<FrameImage> {
        let attempt = |gpu: bool| std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.render_job(job, gpu))).ok();
        attempt(true).or_else(|| {
            log::error!("a frame render panicked (frame {})", job.key.frame);
            job.src.gpu.as_ref().and_then(|_| attempt(false))
        })
    }

    /// A failed scoped attempt retries on CPU. Retired contexts cannot recover by reducing
    /// their budget; only a healthy device's allocation failure uses the upstream backoff.
    fn gpu_attempt_failed(&self, gpu: &Gpu, e: &str) {
        if gpu.context().check_health().is_err() {
            log::warn!("the GPU context is retired ({e}); the frame renders on the CPU");
            return;
        }
        log::warn!("the GPU ran out of memory ({e}); the frame renders on the CPU and fewer GPU frames are kept");
        if let Ok(mut c) = self.cache.lock() {
            c.gpu_out_of_memory();
        }
    }

    /// The frame on the GPU, kept there for the viewer, if the compositor can do it alone.
    fn render_gpu(&self, job: &Job) -> Option<FrameImage> {
        if self.gpu_retired.load(Ordering::Acquire) {
            return None;
        }
        let g = job.src.gpu.as_ref()?;
        let mut r = Renderer::new(&job.src.project, job.src.footage.as_ref(), job.opts);
        r.expr = job.src.expr.as_deref();
        r.cache = Some(&job.src.layer_cache);
        r.accel = Some(g as &dyn effectcraft_engine::render::Accelerator);
        r.active_accel()?;
        match g.within_memory(|| g.render_display(&r, job.comp, job.t)) {
            Ok(f) => f.map(|f| FrameImage::Gpu(Arc::new(f))),
            Err(e) => {
                self.gpu_attempt_failed(g, &e);
                None
            }
        }
    }

    /// Render a frame; `gpu: false` keeps it off the GPU compositor.
    fn render_job(&self, job: &Job, gpu: bool) -> FrameImage {
        let mut r = Renderer::new(&job.src.project, job.src.footage.as_ref(), job.opts);
        r.expr = job.src.expr.as_deref();
        let gpu_allowed = gpu && !self.gpu_retired.load(Ordering::Acquire);
        // A failed GPU attempt may have populated intermediate entries before its scope
        // reported failure. The CPU retry must compute its own pixels from the snapshot.
        r.cache = gpu_allowed.then_some(job.src.layer_cache.as_ref());
        let gpu = job.src.gpu.as_ref().filter(|_| gpu_allowed);
        r.accel = gpu.map(|g| g as &dyn effectcraft_engine::render::Accelerator);
        // The GPU leaves the frame in a texture for the viewer; otherwise (Software Only, no
        // adapter, or a frame the GPU cannot finish here) the CPU renders it.
        // Auto (Mercury GPU Acceleration) shows each comp on whichever compositor measured
        // faster for it (light comps composite faster on the CPU).
        if let (Some(g), Some(a)) = (gpu, r.active_accel())
            && job.src.gpu_display
        {
            // The browser cannot wait for the GPU to time it (and its CPU path is slow): always
            // the GPU there.
            let auto = if cfg!(target_arch = "wasm32") { None } else { r.frame_auto(a, job.comp, true) };
            if auto.is_none_or(|(p, key)| p.choose(key)) {
                let t0 = web_time::Instant::now();
                match g.within_memory(|| g.render_display(&r, job.comp, job.t)) {
                    Ok(Some(f)) => {
                        if let Some((p, key)) = auto {
                            // Time the GPU's work, not just its recording.
                            g.wait();
                            if g.context().check_health().is_err() {
                                return self.render_job(job, false);
                            }
                            p.record(key, true, t0.elapsed().as_secs_f64() * 1000.0);
                        }
                        return FrameImage::Gpu(Arc::new(f));
                    }
                    Ok(None) => {
                        if let Some((p, key)) = auto {
                            p.declined(key);
                        }
                    }
                    Err(e) => {
                        self.gpu_attempt_failed(g, &e);
                        return self.render_job(job, false);
                    }
                }
            }
            let t0 = web_time::Instant::now();
            let img = to_color_image(&r.comp_frame_cpu(job.comp, job.t));
            if let Some((p, key)) = auto {
                p.record(key, false, t0.elapsed().as_secs_f64() * 1000.0);
            }
            return FrameImage::Cpu(Arc::new(img));
        }
        if let Some(g) = gpu {
            // The GPU composites and the frame is read back.
            return match g.within_memory(|| r.comp_frame(job.comp, job.t)) {
                Ok(img) => FrameImage::Cpu(Arc::new(to_color_image(&img))),
                Err(e) => {
                    self.gpu_attempt_failed(g, &e);
                    self.render_job(job, false)
                }
            };
        }
        FrameImage::Cpu(Arc::new(to_color_image(&r.comp_frame(job.comp, job.t))))
    }
}

struct Job {
    key: FrameKey,
    src: RenderSource,
    comp: ItemId,
    t: Tick,
    opts: RenderOpts,
    urgent: bool,
    seq: u64,
}

#[derive(Default)]
struct Queue {
    jobs: Vec<Job>,
    running: HashSet<FrameKey>,
    urgent_running: usize,
    seq: u64,
    /// Spawns that returned because their job had to wait for memory: each finishing frame
    /// starts one again.
    parked: usize,
}

impl Queue {
    fn can_start(&self, job: &Job) -> bool {
        let limit = max_running(job);
        if job.urgent { self.urgent_running == 0 && self.running.len() <= limit } else { self.running.len() < limit }
    }

    /// The job that matters most: urgent first (newest), then prefetch in request order.
    fn best(jobs: &[Job]) -> Option<usize> {
        jobs.iter().enumerate().max_by_key(|(_, j)| (j.urgent, if j.urgent { j.seq as i64 } else { -(j.seq as i64) })).map(|(i, _)| i)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn auxiliary_renders_cannot_nest_and_panics_release_the_shared_slot() {
        let frames = Arc::new(Frames::default());
        let nested = frames.clone();
        let (started, rx) = std::sync::mpsc::channel();
        let (release, wait) = std::sync::mpsc::channel();
        assert!(frames.try_spawn_background(move || {
            started.send(nested.try_spawn_background(|| {})).unwrap();
            wait.recv_timeout(std::time::Duration::from_secs(10)).unwrap();
        }));
        assert!(!rx.recv_timeout(std::time::Duration::from_secs(10)).unwrap(), "a nested pool job cannot acquire a second slot");
        for _ in 0..100 {
            assert!(!frames.try_spawn_background(|| {}), "other selected layers must wait outside the pool");
        }
        release.send(()).unwrap();
        let idle = || {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            while frames.background_running.load(Ordering::Acquire) && std::time::Instant::now() < deadline {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            assert!(!frames.background_running.load(Ordering::Acquire), "the auxiliary slot was not released");
        };
        idle();
        assert!(frames.try_spawn_background(|| panic!("synthetic auxiliary render failure")));
        idle();
        let (done, rx) = std::sync::mpsc::channel();
        assert!(frames.try_spawn_background(move || done.send(()).unwrap()));
        rx.recv_timeout(std::time::Duration::from_secs(10)).unwrap();
        idle();
    }

    /// Frames larger than the texture limit are averaged down by a whole factor (#201: an
    /// 11000×2200 comp at Full resolution against an 8192 limit).
    #[test]
    fn frames_over_the_texture_limit_are_averaged_down() {
        assert_eq!(fitted_size([11000, 2200], 8192), [5500, 1100]);
        assert_eq!(fitted_size([8192, 100], 8192), [8192, 100]);
        assert_eq!(fitted_size([30000, 30000], 16384), [15000, 15000]);
        assert_eq!(fitted_size([0, 0], 0), [0, 0]);
        // 5×2 (three columns of white, then black) into 2 px: 3×2 blocks (the last one 2×2).
        let w = egui::Color32::WHITE;
        let b = egui::Color32::from_rgba_premultiplied(0, 0, 0, 255);
        let img = egui::ColorImage::new([5, 2], vec![w, w, w, b, b, w, w, w, b, b]);
        let out = fit_texture(img.clone(), 2);
        assert_eq!(out.size, [2, 1]);
        assert_eq!(out.pixels, vec![w, b]);
        assert_eq!(fit_texture(img.clone(), 5).pixels, img.pixels);
        let mixed = fit_texture(egui::ColorImage::new([2, 1], vec![w, b]), 1);
        assert_eq!(mixed.pixels, vec![egui::Color32::from_rgba_premultiplied(128, 128, 128, 255)]);
        // A shared frame that fits goes to egui as it is (no pixel copy); one that doesn't is
        // averaged down exactly like an owned one.
        let shared = Arc::new(img.clone());
        assert!(Arc::ptr_eq(&fit_texture_shared(&shared, 5, 1), &shared));
        assert_eq!(fit_texture_shared(&shared, 2, 1).pixels, out.pixels);
        assert_eq!(fit_texture_shared(&shared, 5, 2).pixels, fit_texture_by(img, 5, 2).pixels);
    }

    #[test]
    fn retirement_reaches_existing_workers_and_keeps_cpu_frames_and_completion_live() {
        let frames = Frames::default();
        let worker = frames.worker();
        let first = key(1, 0, 0);
        insert(&frames, first);
        frames.retire_gpu();
        assert!(worker.gpu_retired.load(Ordering::Acquire), "already queued workers share retirement");
        assert!(frames.is_cached(&first), "healthy materialized CPU frames survive");
        let next = key(1, 1, 0);
        frames.inflight.lock().unwrap().insert(next);
        frames.queue.lock().unwrap().running.insert(next);
        worker.finish_key(next, false, img(), web_time::Instant::now());
        assert!(frames.is_cached(&next), "CPU retry can still publish");
        assert_eq!(frames.inflight(), 0);
        assert!(frames.queue.lock().unwrap().running.is_empty());
    }

    fn key(content: u64, frame: i64, opts: u64) -> FrameKey {
        FrameKey { revision: content, content, comp: 1, frame, scale: 1000, view: 0, opts }
    }

    fn img() -> FrameImage {
        FrameImage::Cpu(Arc::new(egui::ColorImage::new([2, 2], vec![egui::Color32::BLACK; 4])))
    }

    fn insert(f: &Frames, k: FrameKey) {
        f.cache.lock().unwrap().insert(k, img());
    }

    /// A frame whose render panics (a GPU out of memory used to, on every frame thread) is
    /// released, not left "in progress" forever, so the viewer can ask for it again (#106).
    #[test]
    fn a_frame_whose_render_panics_is_released() {
        use effectcraft_engine::Session;
        use effectcraft_engine::project::{Footage, FootageKind};
        use effectcraft_engine::render::Image;
        use serde_json::json;
        use std::sync::atomic::{AtomicUsize, Ordering};
        #[derive(Default)]
        struct Explodes(AtomicUsize);
        impl FootageSource for Explodes {
            fn frame(&self, _: ItemId, _: &Footage, _: Tick) -> Option<Arc<Image>> {
                self.0.fetch_add(1, Ordering::SeqCst);
                panic!("a decoder bug");
            }
        }
        let mut s = Session::default();
        let cid = ItemId(s.execute("comp.new", json!({"name": "A", "width": 8, "height": 8, "duration": 1})).unwrap()["comp"].as_u64().unwrap());
        let clip = Footage { kind: FootageKind::Still, width: 8, height: 8, has_video: true, ..Default::default() };
        let item = Arc::make_mut(&mut s.project).add_item("clip.png", Default::default(), None, ItemKind::Footage(clip));
        s.execute("layer.addItem", json!({"item": item.0})).unwrap();
        let footage = Arc::new(Explodes::default());
        let src = RenderSource {
            project: s.project.clone(),
            footage: footage.clone(),
            expr: None,
            layer_cache: Arc::default(),
            gpu: None,
            gpu_display: false,
            disk: None,
        };
        let f = Frames::default();
        let key = FrameKey { revision: s.revision, content: f.content_of(&s.project, s.revision, cid), ..key(0, 0, 0) };
        let key = FrameKey { comp: cid.0, ..key };
        f.request(&src, key, cid, Tick::ZERO, RenderOpts::default());
        for _ in 0..1000 {
            if f.inflight() == 0 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let tries = footage.0.load(Ordering::SeqCst);
        assert!(tries > 0, "the render reached the footage");
        assert_eq!(f.inflight(), 0, "released");
        assert!(!f.is_cached(&key));
        // No hot retry loop: the viewer asks again at every UI frame, but the same frame of the
        // same project isn't rendered (and doesn't panic) again.
        assert!(f.failed(&key));
        for _ in 0..10 {
            f.request(&src, key, cid, Tick::ZERO, RenderOpts::default());
            f.request_urgent(&src, key, cid, Tick::ZERO, RenderOpts::default());
        }
        assert_eq!(f.inflight(), 0, "not queued again");
        assert_eq!(footage.0.load(Ordering::SeqCst), tries);
        // Once the project changes (a later revision), it is tried again.
        let later = FrameKey { revision: key.revision + 1, ..key };
        assert!(!f.failed(&later));
        f.request(&src, later, cid, Tick::ZERO, RenderOpts::default());
        for _ in 0..1000 {
            if f.inflight() == 0 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(footage.0.load(Ordering::SeqCst) > tries, "retried after a change");
        assert!(f.failed(&later));
        // Purge Caches forgets failures too.
        f.clear();
        assert!(!f.failed(&later));
    }

    /// Regression: prefetch frames over the memory budget waited on their pool thread, and rayon
    /// runs queued spawns nested inside renders on the same pool, so every worker could end up
    /// waiting under the render it waited for: nothing finished and playback held at the end of
    /// the cached frames forever. Now they wait in the queue and each finished frame starts the
    /// next, so a long run of frames, one at a time (budget for one), all render.
    #[test]
    fn frames_over_the_memory_budget_wait_in_the_queue_and_all_render() {
        use effectcraft_engine::Session;
        use serde_json::json;
        let mut s = Session::default();
        let cid = ItemId(s.execute("comp.new", json!({"name": "A", "width": 64, "height": 64, "duration": 2})).unwrap()["comp"].as_u64().unwrap());
        s.execute("layer.newSolid", json!({"color": "#ff0000", "width": 32, "height": 32})).unwrap();
        let src = RenderSource {
            project: s.project.clone(),
            footage: Arc::new(effectcraft_engine::render::NoFootage),
            expr: None,
            // Room for one frame at a time.
            layer_cache: Arc::new(LayerCache::new(1)),
            gpu: None,
            gpu_display: false,
            disk: None,
        };
        let f = Frames::default();
        let content = f.content_of(&s.project, s.revision, cid);
        let keys: Vec<FrameKey> = (0..40).map(|i| FrameKey { revision: s.revision, content, comp: cid.0, ..key(0, i, 0) }).collect();
        for (i, k) in keys.iter().enumerate() {
            f.request(&src, *k, cid, Tick::from_seconds_f64(i as f64 / 30.0), RenderOpts::default());
        }
        for _ in 0..3000 {
            if f.inflight() == 0 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(f.inflight(), 0, "every frame finished");
        assert!(keys.iter().all(|k| f.is_cached(k)), "every frame is cached");
    }

    /// Frames rendering at once are bounded by their size against the layer cache budget: 4K
    /// comps render fewer at once than HD ones, and never fewer than one.
    #[test]
    fn concurrent_frames_follow_frame_size_and_budget() {
        use effectcraft_engine::Session;
        use serde_json::json;
        let mut s = Session::default();
        let comp = |s: &mut Session, w: u32, h: u32| {
            ItemId(s.execute("comp.new", json!({"name": "C", "width": w, "height": h, "duration": 1})).unwrap()["comp"].as_u64().unwrap())
        };
        let uhd = comp(&mut s, 3840, 2160);
        let hd = comp(&mut s, 1920, 1080);
        let job = |cid: ItemId, scale: f64, budget: usize| Job {
            key: key(cid.0, 0, 0),
            src: RenderSource {
                project: s.project.clone(),
                footage: Arc::new(effectcraft_engine::render::NoFootage),
                expr: None,
                layer_cache: Arc::new(LayerCache::new(budget)),
                gpu: None,
                gpu_display: false,
                disk: None,
            },
            comp: cid,
            t: Tick::ZERO,
            opts: RenderOpts { scale, ..Default::default() },
            urgent: false,
            seq: 0,
        };
        let gib = 1 << 30;
        assert_eq!(max_running(&job(uhd, 1.0, gib)), 2);
        assert_eq!(max_running(&job(hd, 1.0, gib)), 8);
        assert_eq!(max_running(&job(uhd, 0.5, gib)), 8, "Half resolution renders HD-sized frames");
        assert_eq!(max_running(&job(uhd, 1.0, 64 << 20)), 1);
        assert_eq!(max_running(&job(ItemId(9999), 1.0, gib)), gib, "no comp: nothing to bound");
    }

    /// #647: a viewer frame can jump past busy prefetch, but a drag cannot start an
    /// unbounded stack of obsolete urgent renders through rayons nested job execution.
    #[test]
    fn dragging_keeps_only_one_running_viewer_render() {
        let f = Frames::default();
        let worker = f.worker();
        let src = RenderSource {
            project: Arc::default(),
            footage: Arc::new(effectcraft_engine::render::NoFootage),
            expr: None,
            layer_cache: Arc::new(LayerCache::new(1)),
            gpu: None,
            gpu_display: false,
            disk: None,
        };
        let old = key(1, 0, 0);
        let latest = key(2, 0, 0);
        let mut q = f.queue.lock().unwrap();
        q.running.insert(old);
        q.urgent_running = 1;
        q.jobs.push(Job { key: latest, src, comp: ItemId(1), t: Tick::ZERO, opts: RenderOpts::default(), urgent: true, seq: 2 });
        assert!(!q.can_start(&q.jobs[0]), "one urgent render is already in progress");
        drop(q);
        assert!(!worker.run_next(), "obsolete render must finish before allocating another frame");
        let mut q = f.queue.lock().unwrap();
        assert_eq!(q.jobs.len(), 1, "latest request remains queued");
        q.urgent_running = 0;
        assert!(q.can_start(&q.jobs[0]), "one reserved viewer slot beyond prefetch");
        q.running.insert(key(3, 0, 0));
        assert!(!q.can_start(&q.jobs[0]), "the reserve cannot exceed one frame");
    }

    /// #284: scrubbing asks for a viewer frame at every step; a newer one drops the comp's viewer
    /// frames still waiting (they would all render first), but not prefetch frames or another
    /// comp's viewer frame.
    #[test]
    fn a_newer_viewer_frame_drops_the_ones_still_waiting() {
        /// Starts nothing, so every request stays queued.
        struct Idle;
        impl RemoteFrames for Idle {
            fn slots(&self) -> usize {
                0
            }
            fn start(&self, _: RemoteJob, _: RemoteDone) {}
        }
        let mut f = Frames::default();
        f.set_remote(Some(Arc::new(Idle)));
        let src = RenderSource {
            project: Arc::default(),
            footage: Arc::new(effectcraft_engine::render::NoFootage),
            expr: None,
            layer_cache: Arc::default(),
            gpu: None,
            gpu_display: false,
            disk: None,
        };
        let request = |k: FrameKey, urgent: bool| f.request_with(&src, k, ItemId(k.comp), Tick::ZERO, RenderOpts::default(), urgent);
        let other = FrameKey { comp: 2, ..key(1, 0, 0) };
        request(other, true);
        request(key(1, 20, 0), false);
        for frame in 0..10 {
            request(key(1, frame, 0), true);
        }
        let mut queued: Vec<(u64, i64, bool)> = f.queue.lock().unwrap().jobs.iter().map(|j| (j.key.comp, j.key.frame, j.urgent)).collect();
        queued.sort();
        assert_eq!(queued, [(1, 9, true), (1, 20, false), (2, 0, false)]);
        assert_eq!(f.inflight(), 3, "dropped frames aren't in flight: the viewer can ask again");
    }

    /// Each time the GPU runs out of memory, half as many GPU frames are kept, down to a floor.
    #[test]
    fn running_out_of_video_memory_halves_the_gpu_frame_budget() {
        let f = Frames::default();
        let mut c = f.cache.lock().unwrap();
        assert_eq!(c.gpu_budget, GPU_BUDGET);
        c.gpu_out_of_memory();
        assert_eq!(c.gpu_budget, GPU_BUDGET / 2);
        for _ in 0..20 {
            c.gpu_out_of_memory();
        }
        assert_eq!(c.gpu_budget, MIN_GPU_BUDGET);
    }

    #[test]
    fn least_recently_shown_frames_go_first() {
        let f = Frames::default();
        f.set_budget(3 * 16);
        for i in 0..3 {
            insert(&f, key(1, i, 0));
        }
        // Frame 0 is shown again, so frame 1 is now the oldest.
        assert!(f.get(&key(1, 0, 0)).is_some());
        insert(&f, key(1, 3, 0));
        assert!(f.is_cached(&key(1, 0, 0)) && !f.is_cached(&key(1, 1, 0)));
        assert!(f.is_cached(&key(1, 2, 0)) && f.is_cached(&key(1, 3, 0)));
    }

    /// #595: finishing far-ahead jobs must not evict the next frame playback is waiting for.
    #[test]
    fn playback_window_survives_speculative_frames_within_the_budget() {
        let f = Frames::default();
        f.set_budget(3 * 16);
        f.protect_frames(key(1, 0, 0), [0, 1]);
        insert(&f, key(1, 0, 0));
        insert(&f, key(1, 1, 0));
        for frame in 100..200 {
            insert(&f, key(1, frame, 0));
        }
        assert!(f.is_cached(&key(1, 0, 0)));
        assert!(f.is_cached(&key(1, 1, 0)));
        assert_eq!(f.cache.lock().unwrap().bytes, 3 * 16);
        f.protect_frames(key(1, 0, 0), 0..200);
        for frame in 2..200 {
            insert(&f, key(1, frame, 0));
        }
        assert!(f.is_cached(&key(1, 0, 0)) && f.is_cached(&key(1, 1, 0)), "even when the protected window exceeds memory");
        f.set_budget(16);
        assert!(f.cache.lock().unwrap().bytes <= 16, "protection never exceeds the budget");
        f.protect_frames(key(1, 0, 0), [200, 201]);
        insert(&f, key(1, 200, 0));
        assert!(f.is_cached(&key(1, 200, 0)), "protection follows the playhead");
    }

    /// The cache bar's frame lists are remembered, and follow frames coming and going.
    #[test]
    fn cache_bar_follows_the_cache() {
        let f = Frames::default();
        insert(&f, key(1, 3, 0));
        assert_eq!(f.cached_frames(&key(1, 0, 0)), vec![3]);
        assert_eq!(f.cached_frames(&key(1, 9, 0)), vec![3], "any frame of the series");
        insert(&f, key(1, 1, 0));
        assert_eq!(f.cached_frames(&key(1, 0, 0)), vec![1, 3]);
        f.cache.lock().unwrap().remove(&key(1, 3, 0));
        assert_eq!(f.cached_frames(&key(1, 0, 0)), vec![1]);
        f.clear();
        assert!(f.cached_frames(&key(1, 0, 0)).is_empty());
        // Disk: a frame written to the disk cache shows; one read back into RAM doesn't.
        let dir = std::env::temp_dir().join(format!("effectcraft-cache-bar-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let dc = DiskCache::open(&dir, 1 << 30).unwrap();
        let src = RenderSource {
            project: Arc::default(),
            footage: Arc::new(effectcraft_engine::render::NoFootage),
            expr: None,
            layer_cache: Arc::default(),
            gpu: None,
            gpu_display: false,
            disk: Some(dc.clone()),
        };
        let opts = RenderOpts::default();
        assert!(f.disk_frames(&src, &key(1, 0, 0), 5, &opts).is_empty());
        let k = frame_disk_key(&f.content_keys, &src.project, &key(1, 2, 0), opts_hash(&opts));
        dc.put_frame_now(k, 1, 1, &[0, 0, 0, 255]);
        assert_eq!(f.disk_frames(&src, &key(1, 0, 0), 5, &opts), vec![2]);
        insert(&f, key(1, 2, 0));
        assert!(f.disk_frames(&src, &key(1, 0, 0), 5, &opts).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Only expensive frames go to the disk cache, and prefetch frames only when no other
    /// prefetch is waiting (playback / RAM preview).
    #[test]
    fn disk_cache_keeps_only_expensive_idle_frames() {
        let ms = std::time::Duration::from_millis;
        assert!(!keep_on_disk(ms(2), true, 0), "cheap viewer frame");
        assert!(!keep_on_disk(ms(2), false, 0), "cheap prefetch frame");
        assert!(keep_on_disk(DISK_MIN_RENDER, true, 0), "expensive viewer frame");
        assert!(keep_on_disk(ms(400), true, 12), "the viewer's frame doesn't wait for prefetch");
        assert!(keep_on_disk(ms(400), false, 0), "the last prefetch frame");
        assert!(!keep_on_disk(ms(400), false, 1), "prefetch still queued: playback");
    }

    #[test]
    fn frames_are_keyed_by_content_and_series_are_kept_apart() {
        let f = Frames::default();
        insert(&f, key(1, 0, 0));
        insert(&f, key(2, 0, 0));
        insert(&f, key(2, 1, 7));
        // Other content or render options (Draft, shadows…) are another series.
        assert_eq!(f.cached_frames(&key(2, 0, 0)), vec![0]);
        assert_eq!(f.cached_frames(&key(2, 0, 7)), vec![1]);
        // A later revision with the same content finds the frame (an edit elsewhere, an undo).
        assert!(f.is_cached(&FrameKey { revision: 99, ..key(1, 0, 0) }));
        assert!(!f.is_cached(&key(3, 0, 0)));
    }

    #[test]
    fn live_edits_present_the_newest_completed_revision_without_going_backwards() {
        let f = Frames::default();
        let shown = key(1, 0, 0);
        let newest = key(3, 0, 0);
        let wanted = key(5, 0, 0);
        insert(&f, shown);
        insert(&f, newest);
        insert(&f, key(2, 0, 0)); // Older worker finishes after the newer one.
        let displayed = f.get_for_viewer(&wanted, Some(&shown)).unwrap().0;
        assert_eq!(displayed, newest);
        assert!(f.get_for_viewer(&wanted, Some(&displayed)).is_none(), "never replay an older completion");
        insert(&f, wanted);
        assert_eq!(f.get_for_viewer(&wanted, Some(&displayed)).unwrap().0, wanted, "settle on the exact final edit");
        // Undo reuses the content of an earlier revision immediately.
        let undo = FrameKey { revision: 6, ..shown };
        let reused = f.get_for_viewer(&undo, Some(&wanted)).unwrap().0;
        assert_eq!(reused.content, shown.content);
        assert_eq!(reused.revision, undo.revision);
    }

    #[test]
    fn live_edits_do_not_substitute_other_times_views_or_render_settings() {
        let f = Frames::default();
        let shown = key(1, 0, 0);
        let wanted = key(3, 0, 0);
        let done = key(2, 0, 0);
        for mismatch in [
            FrameKey { comp: 9, ..done },
            FrameKey { frame: 1, ..done },
            FrameKey { scale: 500, ..done },
            FrameKey { view: 1, ..done },
            FrameKey { opts: 1, ..done },
            FrameKey { revision: 4, ..done },
        ] {
            f.clear();
            insert(&f, mismatch);
            assert!(f.get_for_viewer(&wanted, Some(&shown)).is_none(), "wrong candidate: {mismatch:?}");
        }
        f.clear();
        insert(&f, done);
        assert!(f.get_for_viewer(&wanted, None).is_none(), "no stale content on opening a viewer");
        assert!(f.get_for_viewer(&wanted, Some(&FrameKey { comp: 9, ..shown })).is_none());
        assert!(f.get_for_viewer(&wanted, Some(&FrameKey { frame: 1, ..shown })).is_none());
        assert!(f.get_for_viewer(&wanted, Some(&FrameKey { view: 1, ..shown })).is_none());
    }

    #[test]
    fn an_edit_keeps_the_frames_of_comps_it_does_not_touch() {
        use effectcraft_engine::Session;
        use serde_json::json;
        let mut s = Session::default();
        let a = ItemId(s.execute("comp.new", json!({"name": "A", "width": 8, "height": 8, "duration": 1})).unwrap()["comp"].as_u64().unwrap());
        let b = ItemId(s.execute("comp.new", json!({"name": "B", "width": 8, "height": 8, "duration": 1})).unwrap()["comp"].as_u64().unwrap());
        let f = Frames::default();
        let at = |s: &Session, c: ItemId| f.content_of(&s.project, s.revision, c);
        let (a0, b0) = (at(&s, a), at(&s, b));
        // Edit B: A's content (and frames) stay, B's change.
        s.execute("layer.newSolid", json!({"comp": b.0, "color": "#ff0000"})).unwrap();
        assert_eq!(at(&s, a), a0);
        assert_ne!(at(&s, b), b0);
        // Undo: B is back to the content its old frames were rendered for.
        s.execute("edit.undo", json!({})).unwrap();
        assert_eq!(at(&s, b), b0);
    }

    #[test]
    fn a_long_drag_keeps_a_bounded_number_of_states() {
        use effectcraft_engine::Session;
        use serde_json::json;
        let mut s = Session::default();
        let a = ItemId(s.execute("comp.new", json!({"name": "A", "width": 8, "height": 8, "duration": 1})).unwrap()["comp"].as_u64().unwrap());
        let f = Frames::default();
        let mut last = None;
        for i in 0..200 {
            s.execute("comp.settings", json!({"width": 8 + i, "merge": "drag"})).unwrap();
            let k = FrameKey { content: f.content_of(&s.project, s.revision, a), ..key(0, 0, 0) };
            insert(&f, k);
            last = Some(k);
        }
        // The snapshots and the frames kept stay bounded (a dropped identity's frames go with it,
        // so a later state that reuses its addresses can't be shown an old frame).
        assert!(f.keepers.lock().unwrap().1.len() <= MAX_KEEPERS);
        assert!(f.cache.lock().unwrap().map.len() <= MAX_KEEPERS);
        assert!(f.is_cached(&last.unwrap()), "the current state's frame stays");
    }

    #[test]
    fn an_identity_keeps_its_comps_so_an_edit_in_place_still_changes_it() {
        use effectcraft_engine::Session;
        use serde_json::json;
        let mut s = Session::default();
        let a = ItemId(s.execute("comp.new", json!({"name": "A", "width": 8, "height": 8, "duration": 1})).unwrap()["comp"].as_u64().unwrap());
        // A project nothing else holds (no undo history): make_mut would edit the comp in place,
        // at the same address, if the identity hadn't kept a snapshot holding it.
        let mut p = Arc::new((*s.project).clone());
        drop(s);
        let f = Frames::default();
        let before = f.content_of(&p, 1, a);
        let comp = Arc::make_mut(&mut p).comp_mut(a).unwrap();
        comp.width = 16;
        assert_ne!(f.content_of(&p, 2, a), before);
    }

    /// Audio, Lock and Shy (and Hide Shy Layers) don't change pixels: toggling them, in the comp
    /// or a comp it nests, keeps its frames, also when toggled back (#103).
    #[test]
    fn audio_lock_and_shy_switches_keep_the_frames() {
        use effectcraft_engine::Session;
        use serde_json::json;
        let mut s = Session::default();
        let id = |v: serde_json::Value, k: &str| v[k].as_u64().unwrap();
        let inner = ItemId(id(s.execute("comp.new", json!({"name": "Inner", "width": 8, "height": 8, "duration": 1})).unwrap(), "comp"));
        let solid = id(s.execute("layer.newSolid", json!({"comp": inner.0, "color": "#ff0000"})).unwrap(), "layer");
        let outer = ItemId(id(s.execute("comp.new", json!({"name": "Outer", "width": 8, "height": 8, "duration": 1})).unwrap(), "comp"));
        let nested = id(s.execute("layer.addItem", json!({"comp": outer.0, "item": inner.0})).unwrap(), "layer");
        let f = Frames::default();
        let at = |s: &Session| f.content_of(&s.project, s.revision, outer);
        let o0 = at(&s);
        let switch = |s: &mut Session, comp: ItemId, layer: u64, sw: &str, v: bool| {
            s.execute("layer.setSwitch", json!({"comp": comp.0, "layers": [layer], "switch": sw, "value": v})).unwrap();
        };
        for (comp, layer, sw) in [(outer, nested, "audio"), (outer, nested, "lock"), (outer, nested, "shy"), (inner, solid, "audio"), (inner, solid, "shy")] {
            // Audio off; Lock and Shy on.
            switch(&mut s, comp, layer, sw, sw != "audio");
            assert_eq!(at(&s), o0, "{sw} on in {comp:?}");
        }
        s.execute("comp.setSwitch", json!({"comp": outer.0, "switch": "hideShy", "value": true})).unwrap();
        assert_eq!(at(&s), o0, "Hide Shy Layers");
        switch(&mut s, outer, nested, "audio", true);
        switch(&mut s, outer, nested, "lock", false);
        assert_eq!(at(&s), o0, "toggled back");
        // Switches that change pixels still do.
        switch(&mut s, inner, solid, "video", false);
        let o1 = at(&s);
        assert_ne!(o1, o0, "Video");
        switch(&mut s, outer, nested, "audio", false);
        assert_eq!(at(&s), o1, "a mute after a real edit keeps that edit's frames");
        switch(&mut s, inner, solid, "video", true);
        assert_ne!(at(&s), o1);
    }
}
