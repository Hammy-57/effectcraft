//! [`MediaPool`]: decoded footage frames for the compositor, with a memory-budgeted LRU cache.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::thread::ThreadId;

use effectcraft_project::{AlphaMode, Footage, FootageKind, ItemId};
use effectcraft_raster::{Image, Px};
use effectcraft_render::{FootageFrame, FootageSource, SharedFrame, preview_size};
use effectcraft_time::{FrameRate, TICKS_PER_SECOND, Tick};
use filmcraft_media::{FrameRequest, SharedSource};

use crate::convert::{AlphaOp, Packed, pack_dynamic};
use crate::gif_anim::{self, Anim};
use crate::{MediaError, Result};

/// Default frame-cache budget: 1 GiB of decoded frames.
pub const DEFAULT_BUDGET: usize = 1 << 30;

/// The cache path of a missing image-sequence frame (no file can have this name).
const PLACEHOLDER: &str = "\0missing frame";

/// What a missing image-sequence frame shows: 75 % colour bars (white, yellow, cyan, green,
/// magenta, red, blue), the usual "no picture here" frame.
fn colour_bars(w: u32, h: u32) -> Image {
    const BARS: [[f32; 3]; 7] = [[1.0, 1.0, 1.0], [1.0, 1.0, 0.0], [0.0, 1.0, 1.0], [0.0, 1.0, 0.0], [1.0, 0.0, 1.0], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0]];
    let (w, h) = (w.clamp(1, 16_384), h.clamp(1, 16_384));
    let row: Vec<Px> = (0..w)
        .map(|x| {
            let c = BARS.get((x as usize * BARS.len()) / w as usize).copied().unwrap_or([0.0; 3]);
            [c[0] * 0.75, c[1] * 0.75, c[2] * 0.75, 1.0]
        })
        .collect();
    Image { width: w, height: h, data: row.repeat(h as usize) }
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct Key {
    path: Arc<str>,
    frame: i64,
    /// The alpha interpretation (bits 0–1) and Preserve RGB (bit 2): they change the decoded
    /// pixels.
    interp: u8,
    matte: [u32; 3],
    crop: Option<[u64; 4]>,
    /// The preview size a frame is resampled to (reduced-resolution previews).
    preview: Option<[u32; 2]>,
    /// Vector footage drawn at this scale (its bits; 0 = decoded at the native size): one entry
    /// per Continuously Rasterize scale step.
    raster: u64,
}

/// A layer drawn at reduced resolution: its nominal size `w`×`h` at render scale `s` (< 0.75).
#[derive(Clone, Copy, Debug, PartialEq)]
struct Preview {
    w: u32,
    h: u32,
    s: f64,
}

/// A cached frame: float pixels, or the decoder's samples converted on use (8-bit 4:2:0 video
/// takes 1.5 bytes a pixel instead of 16; see [`Packed`]).
#[derive(Clone)]
pub(crate) enum Pixels {
    Float(Arc<Image>),
    Packed(Arc<Packed>),
    /// Resampled to the preview size its key names, as the renderer would.
    Preview(Arc<Image>),
}

impl Pixels {
    /// Bytes held (what the cache budget counts).
    fn bytes(&self) -> usize {
        64 + match self {
            Pixels::Float(img) | Pixels::Preview(img) => img.data.len() * std::mem::size_of::<Px>(),
            Pixels::Packed(p) => p.bytes(),
        }
    }
    /// The frame as compositor pixels (a packed frame is converted now).
    pub(crate) fn image(&self) -> Arc<Image> {
        match self {
            Pixels::Float(img) | Pixels::Preview(img) => img.clone(),
            Pixels::Packed(p) => Arc::new(p.to_image()),
        }
    }
    /// The frame for the renderer to convert into its own buffer.
    fn shared(&self) -> SharedFrame {
        match self {
            Pixels::Float(img) | Pixels::Preview(img) => img.clone(),
            Pixels::Packed(p) => p.clone(),
        }
    }
}

struct Entry {
    px: Pixels,
    stamp: u64,
    bytes: usize,
}

/// Conformed audio files written by builds before this version are ignored (and written again):
/// version 1 files of sources longer than about 12:36 at 48 kHz were silent from there on (#172);
/// version 2 files were written silent where the source failed to decode, and kept the footage
/// silent from then on (#324).
const CONFORM_VERSION: u32 = 3;

/// Footage longer than this (24 hours) is read from its decoder rather than conformed: its
/// duration comes from the file's header, and a damaged one claiming days of audio had the
/// conform thread allocate all of it at once (#408).
const MAX_CONFORM_SECONDS: i64 = 24 * 3600;

/// The sample frames of `footage`'s audio at `rate` Hz that a conformed file holds, `None` past
/// [`MAX_CONFORM_SECONDS`].
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
fn conform_frames(footage: &Footage, rate: u32) -> Option<usize> {
    let frames = footage.duration.to_units_floor(i64::from(rate)).max(0);
    if frames > MAX_CONFORM_SECONDS.saturating_mul(i64::from(rate)) {
        return None;
    }
    usize::try_from(frames).ok()
}

/// The source time of sample frame `at` at `rate` Hz. (`at` × ticks per second overflows `i64`
/// past about 12:36 at 48 kHz.)
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
fn sample_time(at: usize, rate: u32) -> Tick {
    Tick::from_units(i64::try_from(at).unwrap_or(i64::MAX), i64::from(rate))
}

/// Least-recently-used frames, bounded by bytes.
#[derive(Default)]
struct Lru {
    map: HashMap<Key, Entry>,
    order: BTreeMap<u64, Key>,
    clock: u64,
    bytes: usize,
}

impl Lru {
    fn get(&mut self, k: &Key) -> Option<Pixels> {
        let e = self.map.get_mut(k)?;
        self.order.remove(&e.stamp);
        self.clock += 1;
        e.stamp = self.clock;
        self.order.insert(e.stamp, k.clone());
        Some(e.px.clone())
    }
    fn insert(&mut self, k: Key, px: Pixels, budget: usize) {
        let bytes = px.bytes();
        self.clock += 1;
        if let Some(old) = self.map.insert(k.clone(), Entry { px, stamp: self.clock, bytes }) {
            self.order.remove(&old.stamp);
            self.bytes -= old.bytes;
        }
        self.order.insert(self.clock, k);
        self.bytes += bytes;
        self.evict(budget);
    }
    fn remove(&mut self, k: &Key) {
        if let Some(e) = self.map.remove(k) {
            self.order.remove(&e.stamp);
            self.bytes -= e.bytes;
        }
    }
    /// Evict least-recently-used frames until within `budget` (the newest frame always stays).
    fn evict(&mut self, budget: usize) {
        while self.bytes > budget && self.map.len() > 1 {
            let Some((_, k)) = self.order.pop_first() else { break };
            if let Some(e) = self.map.remove(&k) {
                self.bytes -= e.bytes;
            }
        }
    }
}

/// Cache counters (see [`MediaPool::stats`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PoolStats {
    /// Requests answered from the frame cache.
    pub hits: u64,
    /// Requests that had to decode (or wait for a read-ahead decode).
    pub misses: u64,
    /// Frames decoded ahead of sequential reads (playback, export).
    pub prefetched: u64,
    /// Frames currently cached and their total size in bytes.
    pub frames: usize,
    pub bytes: usize,
    /// Movie and audio files opened (each path once, however many callers ask for it at once).
    pub opened: u64,
}

#[derive(Default)]
struct CacheState {
    lru: Lru,
    /// Frames being decoded right now and the thread decoding each (requests for them wait
    /// instead of decoding again).
    inflight: HashMap<Key, ThreadId>,
    /// Last frame index requested per movie (sequential-read detection).
    last: HashMap<Arc<str>, i64>,
    /// Sequential decode-ahead per movie.
    ahead: HashMap<Arc<str>, Ahead>,
    /// Decode-ahead runners started (each runner's id).
    runners: u64,
    /// Counts finished decodes: a waiter that sees no progress for [`STALL`] stops waiting.
    progress: u64,
}

/// A movie's sequential decode-ahead: a runner thread decodes the frames `next..until` (frame
/// indices at the footage's rate, before looping) in order, so readers going through the movie
/// (export workers, playback) find them cached instead of each decoding on its own.
struct Ahead {
    footage: Footage,
    preview: Option<Preview>,
    /// The frames it decodes are keyed like this (frame 0): other interpretations or preview
    /// sizes of the file don't wait for it.
    stream: Key,
    next: i64,
    until: i64,
    /// Bytes of one decoded frame (sizes the window).
    frame_bytes: usize,
    /// The runner working on it (`None`: stopped).
    runner: Option<u64>,
}

/// Requests within this many frames of the previous request of the same movie read it in order
/// (parallel export workers ask for neighbouring frames out of order).
const SEQ_SLACK: i64 = 32;
/// At most this many frames are decoded ahead of a reader …
const MAX_AHEAD: i64 = 32;
/// … and at most this share (1 / n) of the frame-cache budget.
const AHEAD_SHARE: usize = 4;
/// At most this many movies are decoded ahead at once.
const MAX_RUNNERS: usize = 4;
/// Frames a decode-ahead runner of footage with this codec decodes at once: up to 4 (half the
/// cores) for intra-only codecs, whose frames decode independently (FilmCraft decodes them
/// outside its decoder lock); 1 for long-GOP codecs, which decode in order.
fn ahead_batch(codec: &str) -> usize {
    let c = codec.to_ascii_lowercase();
    if !["prores", "dnxh", "motion jpeg", "mjpeg"].iter().any(|n| c.contains(n)) {
        return 1;
    }
    let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
    (cores / 2).clamp(1, 4)
}
/// A rayon worker waits for a frame being decoded elsewhere while decoding progresses, and decodes
/// it itself after this long without progress (see [`Inner::pixels_at`]).
const STALL: std::time::Duration = std::time::Duration::from_secs(1);

/// Whether `a` and `b` key frames of the same stream (all but the frame index match).
fn same_stream(a: &Key, b: &Key) -> bool {
    a.path == b.path && a.interp == b.interp && a.matte == b.matte && a.crop == b.crop && a.preview == b.preview && a.raster == b.raster
}

/// A movie/audio source by path, opened by the first caller while the others wait for it
/// (`None`: failed to open; not retried until `forget`).
type SourceSlot = Arc<OnceLock<Option<SharedSource>>>;

/// At most this many movie/audio files stay open (least recently used closed first).
const MAX_SOURCES: usize = 24;

/// Open files whose decoders hold their whole contents (WAV bytes, decoded MP3 / FLAC samples)
/// are closed beyond this many bytes of such data (least recently used first).
const SOURCE_BUDGET: usize = 1 << 30;

struct SourceEntry {
    slot: SourceSlot,
    stamp: u64,
    /// Bytes the source keeps in memory (estimated once it is open).
    held: usize,
}

/// Open movie/audio files by path, least recently used closed first beyond [`MAX_SOURCES`] or
/// [`SOURCE_BUDGET`].
#[derive(Default)]
struct Sources {
    map: HashMap<Arc<str>, SourceEntry>,
    clock: u64,
}

impl Sources {
    /// The slot of `path` (created empty), marked as just used.
    fn slot(&mut self, path: &Arc<str>) -> SourceSlot {
        self.clock += 1;
        let clock = self.clock;
        let e = self.map.entry(path.clone()).or_insert_with(|| SourceEntry { slot: SourceSlot::default(), stamp: clock, held: 0 });
        e.stamp = clock;
        e.slot.clone()
    }
    /// Whether nothing but this map uses the source of `e` (no decode in progress, nobody waiting
    /// for it to open).
    fn idle(e: &SourceEntry) -> bool {
        Arc::strong_count(&e.slot) == 1 && e.slot.get().is_none_or(|s| s.as_ref().is_none_or(|s| Arc::strong_count(s) == 1))
    }
    /// Close idle sources, least recently used first, until within the count and byte bounds
    /// (`keep` stays open).
    fn trim(&mut self, keep: &str) {
        loop {
            let held: usize = self.map.values().map(|e| e.held).fold(0, usize::saturating_add);
            if self.map.len() <= MAX_SOURCES && held <= SOURCE_BUDGET {
                return;
            }
            let oldest = self.map.iter().filter(|(p, e)| &***p != keep && Self::idle(e)).min_by_key(|(_, e)| e.stamp).map(|(p, _)| p.clone());
            let Some(p) = oldest else { return };
            self.map.remove(&p);
        }
    }
    /// Close every idle source.
    fn close_idle(&mut self) {
        self.map.retain(|_, e| !Self::idle(e));
    }
}

/// At most this many parsed Photoshop documents are kept (each holds its file) …
const MAX_PSDS: usize = 4;
/// … and at most this many bytes of them (the most recent one always stays).
const PSD_BUDGET: usize = 1 << 29;

/// Parsed Photoshop documents, most recently used last: a document imported as a composition
/// has a footage item per layer, and each decoded frame re-read and re-parsed the whole file.
#[derive(Default)]
struct PsdCache {
    list: Vec<(Arc<str>, Arc<effectcraft_psd::Psd>, usize)>,
}

impl PsdCache {
    fn get(&mut self, path: &str) -> Option<Arc<effectcraft_psd::Psd>> {
        let i = self.list.iter().position(|(p, ..)| &**p == path)?;
        let e = self.list.remove(i);
        let psd = e.1.clone();
        self.list.push(e);
        Some(psd)
    }
    fn insert(&mut self, path: Arc<str>, psd: Arc<effectcraft_psd::Psd>, bytes: usize) {
        self.remove(&path);
        self.list.push((path, psd, bytes));
        while self.list.len() > 1 && (self.list.len() > MAX_PSDS || self.list.iter().map(|e| e.2).fold(0, usize::saturating_add) > PSD_BUDGET) {
            self.list.remove(0);
        }
    }
    fn remove(&mut self, path: &str) {
        self.list.retain(|(p, ..)| &**p != path);
    }
}

/// Bytes a source keeps in memory: decoders that read the file whole keep its bytes (WAV) or its
/// decoded samples (MP3, FLAC); containers read through the file keep their index.
fn held_bytes(src: &SharedSource, whole: bool) -> usize {
    if !whole {
        return 1 << 20;
    }
    let info = src.info();
    let file = info.file_size.unwrap_or(0);
    let decoded =
        info.audio.as_ref().map_or(0.0, |a| info.duration.0 as f64 / TICKS_PER_SECOND as f64 * f64::from(a.sample_rate) * f64::from(a.channels) * 4.0);
    usize::try_from(file).unwrap_or(usize::MAX).max(decoded.clamp(0.0, usize::MAX as f64) as usize)
}
/// An animated GIF by path, as [`SourceSlot`] (the error message when it failed to load).
type GifSlot = Arc<OnceLock<std::result::Result<Arc<Anim>, String>>>;

struct Inner {
    /// Opened movie/audio sources by path.
    sources: Mutex<Sources>,
    /// Files opened into `sources` (see [`PoolStats::opened`]).
    opened: AtomicU64,
    /// In-memory files registered with `add_bytes` (web builds, tests).
    files: Mutex<HashMap<String, Arc<[u8]>>>,
    /// Parsed Photoshop documents by path.
    psds: Mutex<PsdCache>,
    /// Parsed 3D models by path (`None`: failed to load; not retried until `forget`).
    models: Mutex<HashMap<String, Option<Arc<effectcraft_model::Model>>>>,
    /// Decoded animated GIFs by path, loaded by the first caller while the others wait (an
    /// `Err` is not retried until `forget`).
    gifs: Mutex<HashMap<Arc<str>, GifSlot>>,
    cache: Mutex<CacheState>,
    done: Condvar,
    budget: AtomicU64,
    read_ahead: AtomicBool,
    hits: AtomicU64,
    misses: AtomicU64,
    prefetched: AtomicU64,
    /// Settings ▸ Disk ▸ Conformed Audio Folder: decoded audio tracks written once per file and
    /// rate, read back instead of decoding again (`None` = off).
    conform: Mutex<Option<std::path::PathBuf>>,
    /// The thread pool decode-ahead runners decode in (started with the first runner; `None`:
    /// could not be started).
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    ahead_pool: OnceLock<Option<rayon::ThreadPool>>,
    /// Conformed files being written right now.
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    conforming: Mutex<HashSet<std::path::PathBuf>>,
    /// Conformed files that failed to write (the source didn't decode) this session: their
    /// audio is read from the decoder instead of trying again on every read.
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    conform_failed: Mutex<HashSet<std::path::PathBuf>>,
}

/// Decodes footage for the renderer: stills, image sequences and movies (FilmCraft codecs), plus
/// audio. Thread-safe and cheap to clone (clones share decoders and cache).
///
/// - One decoder per movie: FilmCraft's GOP cache keeps it positioned, so sequential frames
///   decode without re-seeking, and a seek decodes forward from the preceding sync sample.
/// - Converted frames live in an LRU cache bounded by a byte budget ([`DEFAULT_BUDGET`]).
/// - Reading a movie in order (playback, parallel export) starts a decode-ahead on a runner
///   thread (native builds): it decodes the next frames in order, up to a share of the cache
///   budget, so decoding overlaps with the callers' use of the current frames and they find the
///   next ones cached.
/// - Concurrent requests for the same frame decode it once: the others wait for it.
#[derive(Clone)]
pub struct MediaPool {
    inner: Arc<Inner>,
}

impl Default for MediaPool {
    fn default() -> Self {
        Self::new()
    }
}

/// Where a frame comes from.
struct Loc {
    key: Key,
    /// Media time for movies (`None`: an image file, `key.path`).
    media_t: Option<Tick>,
    /// Resampled for a reduced-resolution preview (`key.preview` is its size).
    preview: Option<Preview>,
}

/// Removes an in-flight key and wakes waiters, also when decoding panics.
struct Inflight<'a> {
    inner: &'a Inner,
    key: Key,
}

impl Drop for Inflight<'_> {
    fn drop(&mut self) {
        let mut c = lock(&self.inner.cache);
        c.inflight.remove(&self.key);
        c.progress = c.progress.wrapping_add(1);
        drop(c);
        self.inner.done.notify_all();
    }
}

impl MediaPool {
    /// A pool with the [`DEFAULT_BUDGET`] frame cache.
    pub fn new() -> Self {
        Self::with_budget(DEFAULT_BUDGET)
    }

    /// A pool whose frame cache holds at most `bytes` of decoded frames.
    pub fn with_budget(bytes: usize) -> Self {
        Self {
            inner: Arc::new(Inner {
                sources: Mutex::default(),
                files: Mutex::default(),
                models: Mutex::default(),
                psds: Mutex::default(),
                gifs: Mutex::default(),
                cache: Mutex::default(),
                done: Condvar::new(),
                budget: AtomicU64::new(bytes as u64),
                read_ahead: AtomicBool::new(cfg!(not(target_arch = "wasm32"))),
                hits: AtomicU64::new(0),
                misses: AtomicU64::new(0),
                prefetched: AtomicU64::new(0),
                opened: AtomicU64::new(0),
                conform: Mutex::default(),
                ahead_pool: OnceLock::new(),
                conforming: Mutex::default(),
                conform_failed: Mutex::default(),
            }),
        }
    }

    pub fn budget(&self) -> usize {
        self.inner.budget()
    }

    /// Change the frame-cache budget (evicts immediately when shrinking).
    pub fn set_budget(&self, bytes: usize) {
        self.inner.budget.store(bytes as u64, Ordering::Relaxed);
        lock(&self.inner.cache).lru.evict(bytes);
    }

    /// Enable or disable decoding ahead of sequential reads (on by default on native builds;
    /// ignored on wasm32).
    pub fn set_read_ahead(&self, on: bool) {
        self.inner.read_ahead.store(on && cfg!(not(target_arch = "wasm32")), Ordering::Relaxed);
    }

    pub fn stats(&self) -> PoolStats {
        let i = &self.inner;
        let c = lock(&i.cache);
        PoolStats {
            hits: i.hits.load(Ordering::Relaxed),
            misses: i.misses.load(Ordering::Relaxed),
            prefetched: i.prefetched.load(Ordering::Relaxed),
            frames: c.lru.map.len(),
            bytes: c.lru.bytes,
            opened: i.opened.load(Ordering::Relaxed),
        }
    }

    /// Drop every cached frame (decoders stay open).
    pub fn clear_frames(&self) {
        let mut c = lock(&self.inner.cache);
        c.lru = Lru::default();
        c.last.clear();
        // Running decode-aheads stop.
        c.ahead.clear();
        drop(c);
        // Decoded GIFs sit outside the frame budget; Purge frees them too.
        lock(&self.inner.gifs).clear();
    }

    /// Drop every cached frame and decoder.
    pub fn clear(&self) {
        self.clear_frames();
        lock(&self.inner.sources).map.clear();
    }

    /// Movie and audio files open now.
    pub fn open_files(&self) -> usize {
        lock(&self.inner.sources).map.len()
    }

    /// Forget every file `keep` doesn't name: close its decoder, drop its parsed model and its
    /// cached frames (after footage is removed from the project, or another project is opened).
    pub fn retain_files(&self, keep: &dyn Fn(&str) -> bool) {
        let gone: Vec<Arc<str>> = lock(&self.inner.sources).map.keys().filter(|p| !keep(p)).cloned().collect();
        for p in &gone {
            self.forget(p);
        }
        lock(&self.inner.models).retain(|p, _| keep(p));
        lock(&self.inner.psds).list.retain(|(p, ..)| keep(p));
    }

    /// Forget a file (after it changed on disk, or to retry a file that failed to open).
    pub fn forget(&self, path: &str) {
        lock(&self.inner.sources).map.remove(path);
        lock(&self.inner.models).remove(path);
        lock(&self.inner.psds).remove(path);
        lock(&self.inner.gifs).remove(path);
        let mut c = lock(&self.inner.cache);
        let keys: Vec<Key> = c.lru.map.keys().filter(|k| &*k.path == path).cloned().collect();
        for k in keys {
            c.lru.remove(&k);
        }
        c.last.remove(path);
        c.ahead.remove(path);
        drop(c);
        crate::exr_channels::forget(path);
    }

    /// Provide the contents of `path` from memory (used instead of the file system; web builds).
    pub fn add_bytes(&self, path: &str, bytes: Arc<[u8]>) {
        lock(&self.inner.files).insert(path.to_string(), bytes);
        self.forget(path);
    }

    /// Decode (or fetch from cache) the frame of `footage` at source time `t`.
    pub fn frame_at(&self, footage: &Footage, t: Tick) -> Result<Arc<Image>> {
        Inner::frame_at(&self.inner, footage, t, true)
    }

    /// Settings ▸ Disk ▸ Conformed Audio Folder: write each footage file's decoded audio there
    /// once per sample rate (raw interleaved stereo `f32`) and read it back instead of decoding
    /// again. `None` turns it off.
    pub fn set_conform_folder(&self, folder: Option<std::path::PathBuf>) {
        *lock(&self.inner.conform) = folder;
    }

    /// The conformed-audio file of `path` at `rate` (named by a hash of the path, size and
    /// modification time, so an edited file conforms again, and of [`CONFORM_VERSION`]).
    pub fn conformed_path(&self, path: &str, rate: u32) -> Option<std::path::PathBuf> {
        use std::hash::{Hash, Hasher};
        let folder = lock(&self.inner.conform).clone()?;
        let meta = std::fs::metadata(path).ok()?;
        let mut h = std::collections::hash_map::DefaultHasher::new();
        (CONFORM_VERSION, path, meta.len(), meta.modified().ok()).hash(&mut h);
        Some(folder.join(format!("{:016x}_{rate}.ecaf", h.finish())))
    }

    /// Samples from a conformed file: `Some` when it exists and covers the request.
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    fn read_conformed(file: &std::path::Path, s0: usize, frames: usize) -> Option<Vec<f32>> {
        use std::io::{Read, Seek, SeekFrom};
        let mut f = std::fs::File::open(file).ok()?;
        let len = f.metadata().ok()?.len() as usize / 8;
        let mut out = vec![0.0f32; frames * 2];
        if s0 >= len {
            return Some(out);
        }
        let n = frames.min(len - s0);
        f.seek(SeekFrom::Start(s0 as u64 * 8)).ok()?;
        let mut bytes = vec![0u8; n * 8];
        f.read_exact(&mut bytes).ok()?;
        for (o, c) in out.iter_mut().zip(bytes.as_chunks::<4>().0.iter()) {
            *o = f32::from_le_bytes([c[0], c[1], c[2], c[3]]);
        }
        Some(out)
    }

    /// Conform `frames` sample frames of `footage`'s audio at `rate` into `file` on a worker
    /// thread (once).
    #[cfg(not(target_arch = "wasm32"))]
    fn conform(&self, footage: &Footage, file: std::path::PathBuf, frames: usize, rate: u32) {
        if lock(&self.inner.conform_failed).contains(&file) || !lock(&self.inner.conforming).insert(file.clone()) {
            return;
        }
        let (pool, f) = (self.clone(), footage.clone());
        let spawned = std::thread::Builder::new().name("ec-conform-audio".into()).spawn(move || {
            let tmp = file.with_extension("tmp");
            let written = match file.parent().map(std::fs::create_dir_all) {
                Some(Ok(())) => pool.write_conformed(&f, &tmp, frames, rate).and_then(|()| std::fs::rename(&tmp, &file)),
                Some(Err(e)) => Err(e),
                None => Err(std::io::Error::other("no folder")),
            };
            // A failed decode leaves no file: one written silent would keep the footage silent
            // in every later session (#324).
            if let Err(e) = written {
                let _ = std::fs::remove_file(&tmp);
                log::warn!("media: could not write conformed audio {} of {}: {e}", file.display(), f.path);
                lock(&pool.inner.conform_failed).insert(file.clone());
            }
            lock(&pool.inner.conforming).remove(&file);
        });
        if spawned.is_err() {
            lock(&self.inner.conforming).clear();
        }
    }

    /// Write `frames` sample frames of `footage`'s audio at `rate` to `path` (raw interleaved
    /// stereo `f32`) a chunk at a time: the whole track was held in memory before it was written.
    /// Fails when any chunk doesn't decode.
    #[cfg(not(target_arch = "wasm32"))]
    fn write_conformed(&self, footage: &Footage, path: &std::path::Path, frames: usize, rate: u32) -> std::io::Result<()> {
        use std::io::Write;
        let mut out = std::io::BufWriter::new(std::fs::File::create(path)?);
        let mut at = 0usize;
        while at < frames {
            let n = (frames - at).min(1 << 16);
            for v in self.try_decode_audio(footage, sample_time(at, rate), n, rate).map_err(std::io::Error::other)? {
                out.write_all(&v.to_le_bytes())?;
            }
            at += n;
        }
        out.flush()
    }

    /// `frames` stereo sample frames of `footage`'s audio starting at source time `start`, at
    /// `rate` Hz, interleaved (L R L R …). Mono is duplicated to both channels; channels beyond
    /// the first two are dropped. Silence where there is no audio (or before the media starts).
    /// With a conformed audio folder, reads come from the conformed file once it is written.
    pub fn audio_samples(&self, footage: &Footage, start: Tick, frames: usize, rate: u32) -> Vec<f32> {
        if !footage.has_audio || footage.missing || rate == 0 || frames == 0 {
            return vec![0.0; frames * 2];
        }
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(conform) = conform_frames(footage, rate)
            && let Some(file) = self.conformed_path(&footage.path, rate)
        {
            let s0 = start.to_units_floor(rate as i64);
            if file.exists() && !lock(&self.inner.conforming).contains(&file) {
                let skip = (-s0).max(0) as usize;
                if skip >= frames {
                    return vec![0.0; frames * 2];
                }
                if let Some(v) = Self::read_conformed(&file, s0.max(0) as usize, frames - skip) {
                    let mut out = vec![0.0; skip * 2];
                    out.extend(v);
                    return out;
                }
            } else {
                self.conform(footage, file, conform, rate);
            }
        }
        self.decode_audio(footage, start, frames, rate)
    }

    /// [`MediaPool::audio_samples`] straight from the decoder. The source is read at its own
    /// rate and resampled here (linear, by absolute sample position, so a range comes out the
    /// same however it is split into reads). FilmCraft's MP4 reader, asked for another rate,
    /// applied the edit list's priming offset twice and ramped the start of every read: AAC
    /// footage not at the mix rate stuttered in previews and exports (#274).
    fn decode_audio(&self, footage: &Footage, start: Tick, frames: usize, rate: u32) -> Vec<f32> {
        self.try_decode_audio(footage, start, frames, rate).unwrap_or_else(|e| {
            log::warn!("media: audio of {}: {e}", footage.path);
            vec![0.0; frames * 2]
        })
    }

    /// [`MediaPool::decode_audio`], failing when the source can't be opened or read.
    fn try_decode_audio(&self, footage: &Footage, start: Tick, frames: usize, rate: u32) -> std::result::Result<Vec<f32>, String> {
        let path: Arc<str> = footage.path.as_str().into();
        let src = self.inner.source(&path).ok_or("the file can't be opened")?;
        let s0 = start.to_units_floor(rate as i64);
        let native = src.info().audio.as_ref().map_or(rate, |a| a.sample_rate);
        if native == 0 || rate == 0 || native == rate {
            return read_stereo(&src, s0, frames, rate);
        }
        // Output sample `n` sits at source position n × native / rate.
        let (native_i, rate_i) = (i128::from(native), i128::from(rate));
        let first = (i128::from(s0) * native_i).div_euclid(rate_i);
        let last = ((i128::from(s0) + frames as i128) * native_i).div_euclid(rate_i) + 1;
        let (Ok(first64), Ok(count)) = (i64::try_from(first), usize::try_from(last - first + 1)) else { return Ok(vec![0.0; frames * 2]) };
        let buf = read_stereo(&src, first64, count, native)?;
        let mut out = Vec::with_capacity(frames * 2);
        for k in 0..frames as i128 {
            let pos = (i128::from(s0) + k) * native_i;
            let i = usize::try_from(pos.div_euclid(rate_i) - first).unwrap_or(0) * 2;
            let f = (pos.rem_euclid(rate_i) as f64 / rate as f64) as f32;
            for c in 0..2 {
                let a = buf.get(i + c).copied().unwrap_or(0.0);
                let b = buf.get(i + 2 + c).copied().unwrap_or(a);
                out.push(a + (b - a) * f);
            }
        }
        Ok(out)
    }

    /// A thumbnail of `footage` (its first frame) fitting in `max_side` × `max_side`, aspect kept.
    /// A cached frame is reused; otherwise the frame is decoded without being cached and a movie
    /// not open already is closed again (browsing a folder doesn't fill memory with its files).
    pub fn thumbnail(&self, footage: &Footage, max_side: u32) -> Result<Image> {
        if !footage.has_video {
            return Err(MediaError::Unsupported(format!("{}: no video", footage.path)));
        }
        let loc = Inner::locate(footage, Tick::ZERO);
        let cached = lock(&self.inner.cache).lru.get(&loc.key);
        let img = match cached {
            Some(px) => px.image(),
            None => self.inner.decode(&loc, footage, true)?.image(),
        };
        let (w, h) = (img.width.max(1), img.height.max(1));
        let s = (max_side.max(1) as f64 / w.max(h) as f64).min(1.0);
        if s >= 1.0 {
            return Ok((*img).clone());
        }
        let (tw, th) = (((w as f64 * s).round() as u32).max(1), ((h as f64 * s).round() as u32).max(1));
        Ok(effectcraft_raster::resample(&img, tw, th))
    }
}

impl Inner {
    fn budget(&self) -> usize {
        self.budget.load(Ordering::Relaxed) as usize
    }

    /// A 3D model file and its sibling resources (buffers, textures, MTL files), parsed once.
    fn model(&self, path: &str) -> Option<Arc<effectcraft_model::Model>> {
        if let Some(m) = lock(&self.models).get(path) {
            return m.clone();
        }
        let loaded = self.read(path).map_err(|e| e.to_string()).and_then(|bytes| {
            let dir = std::path::Path::new(path).parent().map(|d| d.to_path_buf()).unwrap_or_default();
            let resolve = |uri: &str| -> Option<Vec<u8>> {
                let p = dir.join(uri);
                self.read(&p.to_string_lossy()).ok().map(|b| b.to_vec())
            };
            effectcraft_model::load(path, &bytes, &resolve).map_err(|e| e.to_string())
        });
        let m = match loaded {
            Ok(m) => Some(Arc::new(m)),
            Err(e) => {
                log::warn!("media: cannot load model {path}: {e}");
                None
            }
        };
        lock(&self.models).entry(path.to_string()).or_insert(m).clone()
    }

    fn read(&self, path: &str) -> Result<Arc<[u8]>> {
        if let Some(b) = lock(&self.files).get(path) {
            return Ok(b.clone());
        }
        crate::open::read_file(path)
    }

    /// The opened movie/audio source for `path`. The first caller opens it, outside the map's
    /// lock (parsing a large file takes a moment; FLAC, MP3 and AIFF files are decoded whole);
    /// callers asking for the same file meanwhile wait for that instead of opening it again: a
    /// preview's frame workers, audio feeder, conform and waveform threads each decoded a long
    /// FLAC in full at once (#408). Containers are read through the file (index now, samples on
    /// demand), not held in memory.
    fn source(&self, path: &Arc<str>) -> Option<SharedSource> {
        let slot = lock(&self.sources).slot(path);
        let mut held = None;
        let src = slot
            .get_or_init(|| {
                self.opened.fetch_add(1, Ordering::Relaxed);
                let mem = lock(&self.files).get(&**path).cloned();
                match crate::open::open_source(path, mem) {
                    Ok((s, whole)) => {
                        held = Some(held_bytes(&s, whole));
                        Some(s)
                    }
                    Err(e) => {
                        log::warn!("media: cannot open {path}: {e}");
                        None
                    }
                }
            })
            .clone();
        if let Some(h) = held {
            let mut sources = lock(&self.sources);
            if let Some(e) = sources.map.get_mut(path)
                && Arc::ptr_eq(&e.slot, &slot)
            {
                e.held = h;
            }
            drop(slot);
            sources.trim(path);
        }
        src
    }

    /// The source of `path` when it is open already.
    fn open_source(&self, path: &str) -> Option<SharedSource> {
        lock(&self.sources).map.get(path).and_then(|e| e.slot.get().cloned().flatten())
    }

    /// The decoded frames of the animated GIF at `path`. The first caller decodes it, outside the
    /// map's lock; callers asking for the same file meanwhile wait for that.
    fn gif(&self, path: &Arc<str>) -> Result<Arc<Anim>> {
        let slot = lock(&self.gifs).entry(path.clone()).or_default().clone();
        let loaded = slot.get_or_init(|| {
            let loaded = self.read(path).and_then(|bytes| Anim::load(&bytes));
            loaded.map(Arc::new).map_err(|e| {
                log::warn!("media: cannot decode {path}: {e}");
                e.to_string()
            })
        });
        loaded.clone().map_err(|e| MediaError::Decode(format!("{path}: {e}")))
    }

    /// The frame index (in the footage's interpreted rate) shown at source time `t`, after
    /// looping (`loop_count`) and clamping to the footage's frames; `n` = frames per loop.
    fn frame_index(rate: FrameRate, t: Tick, n: i64, loops: u32) -> i64 {
        let n = n.max(1);
        let total = n.saturating_mul(loops.max(1) as i64);
        rate.frame_at(t).clamp(0, total - 1) % n
    }

    /// Number of frames in `d` at `rate` (rounded).
    fn frame_count(rate: FrameRate, d: Tick) -> i64 {
        let den = TICKS_PER_SECOND as i128 * rate.den as i128;
        ((d.0 as i128 * rate.num as i128 + den / 2) / den) as i64
    }

    fn locate(footage: &Footage, t: Tick) -> Loc {
        let alpha = match footage.alpha {
            AlphaMode::Straight => 0,
            AlphaMode::Premultiplied => 1,
            AlphaMode::Ignore => 2,
        };
        let interp = alpha | (u8::from(footage.preserve_rgb) << 2);
        let matte = if footage.alpha == AlphaMode::Premultiplied { footage.premul_color.map(f32::to_bits) } else { [0; 3] };
        let crop = footage.layer.as_ref().and_then(|l| l.crop_bounds).map(|b| b.map(f64::to_bits));
        let key = |path: &str, frame| Key { path: path.into(), frame, interp, matte, crop, preview: None, raster: 0 };
        match footage.kind {
            FootageKind::Sequence if !footage.sequence.is_empty() => {
                let i = Self::frame_index(footage.frame_rate, t, footage.sequence_frames(), footage.loop_count);
                match footage.sequence_file(i) {
                    Some(path) => Loc { key: key(path, 0), media_t: None, preview: None },
                    // A gap in the numbering: colour bars, as in After Effects.
                    None => Loc { key: key(PLACEHOLDER, ((footage.width as i64) << 32) | footage.height as i64), media_t: None, preview: None },
                }
            }
            // A layer of a layered still is keyed by its layer index and size mode (smart objects
            // by their embedded file; PDF pages by their number).
            FootageKind::Still if footage.layer.is_some() || footage.page != 0 => {
                let l = footage.layer.as_ref().map_or(0, |l| 1 + l.index as i64 * 2 + l.layer_size as i64 + if l.embedded.is_some() { 1 << 40 } else { 0 })
                    + ((footage.page as i64) << 42);
                Loc { key: key(&footage.path, l), media_t: None, preview: None }
            }
            FootageKind::Still | FootageKind::Sequence | FootageKind::Model | FootageKind::Data => {
                Loc { key: key(&footage.path, 0), media_t: None, preview: None }
            }
            FootageKind::Video | FootageKind::Audio => {
                let rate = footage.frame_rate;
                let n = Self::frame_count(rate, footage.duration);
                let i = Self::frame_index(rate, t, n, footage.loop_count);
                // Middle of the frame in the file's own timing (robust to timestamp rounding).
                let native = footage.native_rate.unwrap_or(rate);
                let mt = native.tick_of(i) + Tick(native.frame_duration().0 / 2);
                Loc { key: key(&footage.path, i), media_t: Some(mt), preview: None }
            }
        }
    }

    fn frame_at(self: &Arc<Self>, footage: &Footage, t: Tick, playback: bool) -> Result<Arc<Image>> {
        Ok(self.pixels_at(footage, t, playback, None)?.image())
    }

    /// Where the frame of `footage` at `t` comes from, resampled for `preview` when the footage's
    /// frames are larger than the preview needs.
    fn locate_for(footage: &Footage, t: Tick, preview: Option<Preview>) -> Loc {
        let mut loc = Self::locate(footage, t);
        if let Some(p) = preview
            && let Some((w, h)) = preview_size(footage.width, p.w, p.h, p.s)
        {
            loc.key.preview = Some([w, h]);
            loc.preview = Some(p);
        }
        loc
    }

    /// The cached (or newly decoded) pixels of `footage` at source time `t` (resampled for
    /// `preview`).
    fn pixels_at(self: &Arc<Self>, footage: &Footage, t: Tick, playback: bool, preview: Option<Preview>) -> Result<Pixels> {
        if !footage.has_video {
            return Err(MediaError::Unsupported(format!("{}: no video", footage.path)));
        }
        let loc = Self::locate_for(footage, t, preview);
        let raw = footage.frame_rate.frame_at(t);
        let mut c = lock(&self.cache);
        // a movie read in order: decode the next frames ahead
        if playback && loc.media_t.is_some() && !footage.codec.eq_ignore_ascii_case("gif") && self.read_ahead.load(Ordering::Relaxed) {
            self.schedule(&mut c, footage, preview, &loc.key, raw);
        }
        let me = std::thread::current().id();
        // (last progress seen, when it was seen): rayon workers stop waiting without progress
        let mut seen: Option<(u64, std::time::Instant)> = None;
        loop {
            if let Some(px) = c.lru.get(&loc.key) {
                self.hits.fetch_add(1, Ordering::Relaxed);
                return Ok(px);
            }
            let owner = c.inflight.get(&loc.key).copied();
            let scheduled = owner.is_none()
                && c.ahead.get(&loc.key.path).is_some_and(|a| a.runner.is_some() && same_stream(&a.stream, &loc.key) && (a.next..a.until).contains(&raw));
            if owner.is_none() && !scheduled {
                break;
            }
            // This thread is decoding the frame further down its stack (a rayon worker that took
            // this job while its decoder waited on a parallel join): waiting would wait on itself.
            if owner == Some(me) {
                self.misses.fetch_add(1, Ordering::Relaxed);
                drop(c);
                return self.decode(&loc, footage, false);
            }
            // Another thread or the decode-ahead is decoding the frame: wait for it. A rayon
            // worker waits only while decoding progresses: the decoding thread may wait on a
            // parallel job queued behind this worker's own stack, which it can't run while it
            // waits here. Without progress for a while it decodes a private copy instead.
            if rayon::current_thread_index().is_some() {
                let now = std::time::Instant::now();
                let since = match seen {
                    Some((p, at)) if p == c.progress => at,
                    _ => now,
                };
                seen = Some((c.progress, since));
                let left = STALL.saturating_sub(now.saturating_duration_since(since));
                if left.is_zero() {
                    self.misses.fetch_add(1, Ordering::Relaxed);
                    drop(c);
                    return self.decode(&loc, footage, false);
                }
                c = match self.done.wait_timeout(c, left) {
                    Ok((g, _)) => g,
                    Err(e) => e.into_inner().0,
                };
            } else {
                c = self.done.wait(c).unwrap_or_else(|e| e.into_inner());
            }
        }
        self.misses.fetch_add(1, Ordering::Relaxed);
        c.inflight.insert(loc.key.clone(), me);
        drop(c);
        let _guard = Inflight { inner: self, key: loc.key.clone() };
        let px = self.decode(&loc, footage, false)?;
        lock(&self.cache).lru.insert(loc.key, px.clone(), self.budget());
        Ok(px)
    }

    /// A read of frame `raw` (key `key`) of a movie: when it follows the previous read closely,
    /// extend the movie's decode-ahead past it (starting a runner when none is running).
    fn schedule(self: &Arc<Self>, c: &mut CacheState, footage: &Footage, preview: Option<Preview>, key: &Key, raw: i64) {
        let prev = c.last.insert(key.path.clone(), raw);
        if !prev.is_some_and(|p| p != raw && p.abs_diff(raw) <= SEQ_SLACK.unsigned_abs()) {
            return;
        }
        let estimate = (footage.width as usize).saturating_mul(footage.height as usize).saturating_mul(3) / 2 + 64;
        let frame_bytes = c.ahead.get(&key.path).filter(|a| same_stream(&a.stream, key)).map_or(estimate, |a| a.frame_bytes).max(1);
        let window = i64::try_from(self.budget() / AHEAD_SHARE / frame_bytes).unwrap_or(MAX_AHEAD).min(MAX_AHEAD);
        if window < 1 {
            return;
        }
        let total = Self::frame_count(footage.frame_rate, footage.duration).max(1).saturating_mul(i64::from(footage.loop_count.max(1)));
        let until = raw.saturating_add(1).saturating_add(window).min(total);
        let running = c.ahead.values().filter(|a| a.runner.is_some()).count();
        let a = c.ahead.entry(key.path.clone()).or_insert_with(|| Ahead {
            footage: footage.clone(),
            preview,
            stream: Key { frame: 0, ..key.clone() },
            next: raw.saturating_add(1),
            until,
            frame_bytes,
            runner: None,
        });
        let jump = raw < a.next.saturating_sub(SEQ_SLACK) || raw >= a.until;
        if !same_stream(&a.stream, key) || a.footage != *footage || a.preview != preview || a.runner.is_none() || jump {
            // another stream of the file, a stopped runner or a jump: start over after this frame,
            // with a window from here (keeping the old far end after a backward jump or a loop
            // wrap decoded the whole clip past the window and the memory budget)
            if !same_stream(&a.stream, key) {
                a.frame_bytes = estimate;
            }
            a.footage = footage.clone();
            a.preview = preview;
            a.stream = Key { frame: 0, ..key.clone() };
            a.next = raw.saturating_add(1);
            a.until = until;
        }
        a.until = a.until.max(until).min(total);
        if a.runner.is_some() || a.next >= a.until || running >= MAX_RUNNERS {
            return;
        }
        c.runners = c.runners.wrapping_add(1);
        let id = c.runners;
        if self.start_runner(key.path.clone(), id)
            && let Some(a) = c.ahead.get_mut(&key.path)
        {
            a.runner = Some(id);
        }
    }

    /// Start the decode-ahead runner `id` of `path`: a plain thread, not a rayon job (rayon
    /// workers wait for the frames it decodes). It decodes in its own thread pool, so codecs that
    /// parallelise with rayon don't queue their work behind the global pool's waiting workers.
    #[cfg(not(target_arch = "wasm32"))]
    fn start_runner(self: &Arc<Self>, path: Arc<str>, id: u64) -> bool {
        let threads = std::thread::available_parallelism().map_or(4, |n| n.get());
        let pool =
            self.ahead_pool.get_or_init(|| rayon::ThreadPoolBuilder::new().num_threads(threads).thread_name(|i| format!("ec-media-ahead-{i}")).build().ok());
        if pool.is_none() {
            return false;
        }
        let weak = Arc::downgrade(self);
        std::thread::Builder::new()
            .name("ec-media-decode-ahead".into())
            .spawn(move || {
                let _stop = RunnerStop { inner: weak.clone(), path: path.clone(), id };
                while let Some(inner) = weak.upgrade() {
                    if !inner.run_ahead(&path, id) {
                        return;
                    }
                }
            })
            .is_ok()
    }

    #[cfg(target_arch = "wasm32")]
    fn start_runner(self: &Arc<Self>, _path: Arc<str>, _id: u64) -> bool {
        false
    }

    /// One step of runner `id` of `path`: decode its next frames (skipping frames cached or being
    /// decoded already), [`ahead_batch`] of them at once for intra-only codecs, then cache them
    /// in frame order. `false` when the runner is done (window decoded, stopped or replaced).
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    fn run_ahead(&self, path: &Arc<str>, id: u64) -> bool {
        let mut c = lock(&self.cache);
        let Some(a) = c.ahead.get_mut(path).filter(|a| a.runner == Some(id)) else { return false };
        if a.next >= a.until {
            a.runner = None;
            drop(c);
            self.done.notify_all();
            return false;
        }
        let (footage, preview) = (a.footage.clone(), a.preview);
        let batch = ahead_batch(&footage.codec);
        let rate = footage.frame_rate;
        // (frame index, where it comes from) of the frames this step decodes
        let mut todo: Vec<(i64, Loc)> = Vec::with_capacity(batch);
        while todo.len() < batch {
            let Some(a) = c.ahead.get_mut(path) else { break };
            if a.next >= a.until {
                break;
            }
            let raw = a.next;
            a.next = a.next.saturating_add(1);
            let t = rate.tick_of(raw) + Tick(rate.frame_duration().0 / 2);
            let loc = Self::locate_for(&footage, t, preview);
            if c.lru.map.contains_key(&loc.key) || c.inflight.contains_key(&loc.key) || todo.iter().any(|(_, l)| l.key == loc.key) {
                continue;
            }
            todo.push((raw, loc));
        }
        if todo.is_empty() {
            return true;
        }
        let me = std::thread::current().id();
        for (_, loc) in &todo {
            c.inflight.insert(loc.key.clone(), me);
        }
        drop(c);
        let guards: Vec<Inflight> = todo.iter().map(|(_, loc)| Inflight { inner: self, key: loc.key.clone() }).collect();
        let decode_all = || -> Vec<Result<Pixels>> {
            match todo.len() {
                1 => todo.iter().map(|(_, loc)| self.decode(loc, &footage, false)).collect(),
                _ => {
                    use rayon::prelude::*;
                    todo.par_iter().map(|(_, loc)| self.decode(loc, &footage, false)).collect()
                }
            }
        };
        let decoded = match self.ahead_pool.get().and_then(Option::as_ref) {
            Some(pool) => pool.install(decode_all),
            None => decode_all(),
        };
        let mut c = lock(&self.cache);
        // in frame order, up to the first frame that failed (as decoding one at a time would)
        for ((raw, loc), px) in todo.into_iter().zip(decoded) {
            match px {
                Ok(px) => {
                    let bytes = px.bytes();
                    c.lru.insert(loc.key, px, self.budget());
                    if let Some(a) = c.ahead.get_mut(path).filter(|a| a.runner == Some(id)) {
                        a.frame_bytes = bytes;
                    }
                    self.prefetched.fetch_add(1, Ordering::Relaxed);
                }
                Err(e) => {
                    log::debug!("media: decode-ahead of {path} at frame {raw}: {e}");
                    if let Some(a) = c.ahead.get_mut(path).filter(|a| a.runner == Some(id)) {
                        a.until = a.next;
                    }
                    break;
                }
            }
        }
        drop(c);
        drop(guards);
        true
    }

    /// Decode the frame at `loc`: movies and 8 / 16-bit stills are kept as their samples
    /// ([`Packed`]), float stills, layered and vector documents as float pixels.
    ///
    /// `detached`: a movie not open already is opened for this frame only (thumbnails).
    fn decode(&self, loc: &Loc, footage: &Footage, detached: bool) -> Result<Pixels> {
        let _profile = effectcraft_render::profile::span("decode");
        // A reduced-resolution preview: the full frame (cached, or decoded and not kept)
        // resampled exactly as the renderer resamples it.
        if let (Some(p), Some([rw, rh])) = (loc.preview, loc.key.preview) {
            let base = Loc { key: Key { preview: None, ..loc.key.clone() }, media_t: loc.media_t, preview: None };
            let cached = lock(&self.cache).lru.get(&base.key);
            let full = match cached {
                Some(px) => px,
                // The full frame, decoded and not kept, then resampled exactly as the renderer
                // resamples it (bit-identical to a full-size render scaled down; the faster
                // quarter-pixel 4:2:0 conversion differs where colours clip).
                None => match base.media_t.filter(|_| !gif_anim::is_gif(footage)) {
                    Some(mt) => {
                        let frame = self.video_frame(&base.key.path, mt, detached)?;
                        Pixels::Packed(Arc::new(Packed::Video { frame, op: AlphaOp::of(footage) }))
                    }
                    None => self.decode(&base, footage, detached)?,
                },
            };
            let img = full.image();
            if preview_size(img.width, p.w, p.h, p.s).is_none() {
                return Ok(full);
            }
            return Ok(Pixels::Preview(Arc::new(effectcraft_raster::resample(&img, rw, rh))));
        }
        let op = AlphaOp::of(footage);
        let path = &loc.key.path;
        let float = |img: Image| Pixels::Float(Arc::new(img));
        match loc.media_t {
            None if &**path == PLACEHOLDER => Ok(float(colour_bars(footage.width, footage.height))),
            None => {
                // Photoshop documents are parsed once per file, not once per layer item.
                if let Some(psd) = lock(&self.psds).get(path) {
                    return Ok(float(crate::layered::decode_psd(path, &psd, footage, op)?));
                }
                let bytes = self.read(path)?;
                if effectcraft_psd::is_psd(&bytes) {
                    let psd = Arc::new(crate::layered::parse_psd(path, &bytes)?);
                    if !detached {
                        lock(&self.psds).insert(path.clone(), psd.clone(), bytes.len());
                    }
                    return Ok(float(crate::layered::decode_psd(path, &psd, footage, op)?));
                }
                if let Some(img) = crate::layered::decode(path, &bytes, footage, op)? {
                    return Ok(float(img));
                }
                if let Some(img) = crate::avif::decode(path, &bytes, op)? {
                    return Ok(float(img));
                }
                // (an OpenEXR compression the decoder lacks came out transparent or as a missing
                // channel error)
                if path.to_ascii_lowercase().ends_with(".exr")
                    && let Some(why) = crate::exr_channels::unsupported_compression(&bytes)
                {
                    return Err(MediaError::Decode(format!("{path}: {why}")));
                }
                let img = match image::load_from_memory(&bytes) {
                    Ok(img) => img,
                    // A multi-layer OpenEXR file without an unnamed RGB layer: its colour layer.
                    Err(e) => crate::exr_channels::cached_layered_image(path, bytes.clone()).ok_or_else(|| MediaError::Decode(format!("{path}: {e}")))?,
                };
                drop(bytes);
                Ok(match pack_dynamic(img, op) {
                    Ok(img) => float(img),
                    Err(packed) => Pixels::Packed(Arc::new(packed)),
                })
            }
            Some(mt) if gif_anim::is_gif(footage) => {
                // A thumbnail decodes the GIF without keeping it.
                let anim = if detached { Arc::new(Anim::load(&self.read(path)?)?) } else { self.gif(path)? };
                let frame = anim.frame_at(Tick(mt.0)).ok_or_else(|| MediaError::Decode(format!("{path}: no frames")))?;
                Ok(match pack_dynamic(frame.clone(), op) {
                    Ok(img) => float(img),
                    Err(packed) => Pixels::Packed(Arc::new(packed)),
                })
            }
            Some(mt) => {
                let frame = self.video_frame(path, mt, detached)?;
                Ok(Pixels::Packed(Arc::new(Packed::Video { frame, op })))
            }
        }
    }

    /// The decoded movie frame of `path` at media time `mt`. `detached`: a movie not open already
    /// is opened for this frame only.
    fn video_frame(&self, path: &Arc<str>, mt: Tick, detached: bool) -> Result<Arc<filmcraft_frame::VideoFrame>> {
        let src = match detached {
            false => self.source(path),
            true => self.open_source(path).or_else(|| {
                let mem = lock(&self.files).get(&**path).cloned();
                crate::open::open_source(path, mem).ok().map(|(s, _)| s)
            }),
        }
        .ok_or_else(|| MediaError::Io(format!("{path}: cannot open")))?;
        src.video_frame(FrameRequest::full(filmcraft_time::Tick(mt.0))).map_err(MediaError::from)
    }
}

impl FootageSource for MediaPool {
    fn set_conform_folder(&self, folder: Option<std::path::PathBuf>) {
        MediaPool::set_conform_folder(self, folder);
    }
    fn model(&self, _item: ItemId, footage: &Footage) -> Option<Arc<effectcraft_model::Model>> {
        if footage.missing || footage.kind != FootageKind::Model {
            return None;
        }
        self.inner.model(&footage.path)
    }
    fn set_cache_budget(&self, bytes: usize) {
        self.set_budget(bytes);
    }
    fn purge(&self) {
        self.clear_frames();
        lock(&self.inner.sources).close_idle();
    }
    fn retain_files(&self, keep: &dyn Fn(&str) -> bool) {
        MediaPool::retain_files(self, keep);
    }
    fn forget(&self, path: &str) {
        MediaPool::forget(self, path);
    }
    fn thumbnail(&self, footage: &Footage, max_side: u32) -> Option<Image> {
        if footage.missing {
            return None;
        }
        MediaPool::thumbnail(self, footage, max_side).map_err(|e| log::debug!("media: thumbnail of {}: {e}", footage.path)).ok()
    }
    fn cache_budget(&self) -> Option<usize> {
        Some(self.budget())
    }
    fn frame(&self, _item: ItemId, footage: &Footage, t: Tick) -> Option<Arc<Image>> {
        self.pixels(footage, t, None).map(|px| px.image())
    }
    fn frame_pixels(&self, _item: ItemId, footage: &Footage, t: Tick) -> Option<SharedFrame> {
        self.pixels(footage, t, None).map(|px| px.shared())
    }
    fn preview_frame(&self, _item: ItemId, footage: &Footage, t: Tick, w: u32, h: u32, scale: f64) -> Option<FootageFrame> {
        let px = self.pixels(footage, t, Some(Preview { w, h, s: scale }))?;
        Some(FootageFrame { preview: matches!(px, Pixels::Preview(_)), px: px.shared() })
    }

    fn vector_frame(&self, _item: ItemId, footage: &Footage, scale: f64) -> Option<Arc<Image>> {
        if footage.missing || !effectcraft_render::is_vector_footage(footage) {
            return None;
        }
        if !(scale.is_finite() && scale > 0.0) {
            return None;
        }
        // Cached per scale (the renderer asks in quarter-octave steps), with the decoded frames:
        // the same budget, and `forget` drops them when the file changes.
        let key = Key { raster: scale.to_bits(), ..Inner::locate(footage, Tick::ZERO).key };
        if let Some(px) = lock(&self.inner.cache).lru.get(&key) {
            self.inner.hits.fetch_add(1, Ordering::Relaxed);
            return Some(px.image());
        }
        self.inner.misses.fetch_add(1, Ordering::Relaxed);
        let bytes = self.inner.read(&footage.path).ok()?;
        let img = Arc::new(crate::layered::rasterize_vector(&footage.path, &bytes, footage.layer.as_ref(), footage.page, scale)?);
        lock(&self.inner.cache).lru.insert(key, Pixels::Float(img.clone()), self.inner.budget());
        Some(img)
    }

    fn aux(&self, _item: ItemId, footage: &Footage, t: Tick) -> Option<Arc<effectcraft_raster::AuxChannels>> {
        if footage.missing || !footage.has_video {
            return None;
        }
        let loc = Inner::locate(footage, t);
        if loc.media_t.is_some() || !loc.key.path.to_ascii_lowercase().ends_with(".exr") {
            return None;
        }
        let path = loc.key.path.clone();
        crate::exr_channels::cached(&path, || self.inner.read(&path).ok())
    }

    fn audio(&self, _item: ItemId, footage: &Footage, t: Tick, frames: usize, rate: u32) -> Option<Vec<f32>> {
        if !footage.has_audio || footage.missing {
            return None;
        }
        Some(self.audio_samples(footage, t, frames, rate))
    }
}

/// Marks a decode-ahead runner stopped when its thread ends (also when a decode panics), and
/// wakes the readers waiting for its frames.
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
struct RunnerStop {
    inner: std::sync::Weak<Inner>,
    path: Arc<str>,
    id: u64,
}

impl Drop for RunnerStop {
    fn drop(&mut self) {
        let Some(inner) = self.inner.upgrade() else { return };
        if let Some(a) = lock(&inner.cache).ahead.get_mut(&self.path).filter(|a| a.runner == Some(self.id)) {
            a.runner = None;
        }
        inner.done.notify_all();
    }
}

impl MediaPool {
    /// The pixels of `footage` at `t` for the renderer (playback read-ahead on), logged and
    /// `None` when they can't be decoded.
    fn pixels(&self, footage: &Footage, t: Tick, preview: Option<Preview>) -> Option<Pixels> {
        if footage.missing {
            return None;
        }
        match Inner::pixels_at(&self.inner, footage, t, true, preview) {
            Ok(px) => Some(px),
            Err(e) => {
                log::warn!("media: frame of {} at {:?}: {e}", footage.path, t);
                None
            }
        }
    }
}

/// `frames` stereo sample frames (interleaved) of `src` from sample `s0` at `rate` Hz, which
/// FilmCraft serves as decoded when it is the source's own rate. Mono is duplicated to both
/// channels; silence before the start. An error where decoding fails.
fn read_stereo(src: &SharedSource, s0: i64, frames: usize, rate: u32) -> std::result::Result<Vec<f32>, String> {
    let mut out = vec![0.0; frames * 2];
    let skip = usize::try_from(s0.saturating_neg()).unwrap_or(0);
    if skip >= frames {
        return Ok(out);
    }
    let buf = src.audio(s0.max(0), frames - skip, rate).map_err(|e| e.to_string())?;
    let (Some(l), Some(r)) = (buf.channels.first(), buf.channels.get(1).or(buf.channels.first())) else { return Ok(out) };
    let Some(dst) = skip.checked_mul(2).and_then(|i| out.get_mut(i..)) else { return Ok(out) };
    for ([o0, o1], (a, b)) in dst.as_chunks_mut::<2>().0.iter_mut().zip(l.iter().zip(r)) {
        *o0 = *a;
        *o1 = *b;
    }
    Ok(out)
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Conforming a long file's audio reads every chunk at its own time: the sample index times
    /// ticks per second overflowed `i64` past 12:36 at 48 kHz, and later chunks came out silent
    /// (#172).
    #[test]
    fn conform_chunks_keep_their_times_on_long_files() {
        for rate in [24_000u32, 44_100, 48_000, 96_000] {
            let mut last = Tick(-1);
            // Every chunk start of a 2-hour file.
            for at in (0..rate as usize * 7200).step_by(1 << 16) {
                let t = sample_time(at, rate);
                assert!(t > last, "{rate} Hz, frame {at}: {t:?} after {last:?}");
                assert!((t.seconds() - at as f64 / rate as f64).abs() < 1e-6, "{rate} Hz, frame {at}");
                last = t;
            }
        }
        assert!((sample_time(36_372_480, 48_000).seconds() - 757.76).abs() < 1e-9);
    }

    /// #408: footage whose header claims a month of audio isn't conformed: the conform thread
    /// allocated the whole conformed track up front (a terabyte here), and the failed allocation
    /// aborted the app. Its audio is read from the decoder instead (silence: this file doesn't
    /// decode).
    #[test]
    fn a_header_claiming_a_month_of_audio_is_not_conformed() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/test-conform-month");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("month.flac");
        std::fs::write(&path, b"fLaC, not really").unwrap();
        let footage = Footage {
            path: path.to_string_lossy().into(),
            kind: FootageKind::Audio,
            duration: Tick::from_seconds_f64(30.0 * 86_400.0),
            has_audio: true,
            ..Default::default()
        };
        assert_eq!(conform_frames(&footage, 48_000), None);
        assert_eq!(conform_frames(&Footage { duration: Tick::from_seconds_f64(60.0), ..footage.clone() }, 48_000), Some(2_880_000));
        let pool = MediaPool::new();
        let conformed = dir.join("conformed");
        pool.set_conform_folder(Some(conformed.clone()));
        let s = pool.audio_samples(&footage, Tick::from_seconds_f64(3600.0), 1024, 48_000);
        assert_eq!(s.len(), 2048);
        assert!(s.iter().all(|v| *v == 0.0));
        assert!(lock(&pool.inner.conforming).is_empty(), "no conform thread started");
        assert!(!conformed.exists(), "nothing was written");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Open files are bounded (least recently used closed first), purging closes the idle ones,
    /// and files the project no longer names are closed.
    #[test]
    fn open_files_are_bounded_and_released() {
        let pool = MediaPool::new();
        let wav: Arc<[u8]> = filmcraft_media::wav::write_wav16(&[0.25; 9600], 2, 48_000).into();
        let footage = |i: usize| {
            let path = format!("/mem/tone{i}.wav");
            pool.add_bytes(&path, wav.clone());
            Footage { path, kind: FootageKind::Audio, duration: Tick::from_seconds_f64(0.1), has_audio: true, ..Default::default() }
        };
        let files: Vec<Footage> = (0..MAX_SOURCES + 6).map(footage).collect();
        for f in &files {
            assert!(pool.audio_samples(f, Tick::ZERO, 16, 48_000).iter().all(|v| (*v - 0.25).abs() < 1e-3));
        }
        assert_eq!(pool.open_files(), MAX_SOURCES);
        assert!(lock(&pool.inner.sources).map.contains_key(files[MAX_SOURCES + 5].path.as_str()), "the newest stays open");
        assert!(!lock(&pool.inner.sources).map.contains_key(files[0].path.as_str()), "the oldest was closed");
        // Reopened on demand.
        assert!(pool.audio_samples(&files[0], Tick::ZERO, 16, 48_000).iter().all(|v| (*v - 0.25).abs() < 1e-3));
        pool.retain_files(&|p| p == files[1].path || p == files[2].path || p == files[0].path);
        assert!(pool.open_files() <= 3);
        // A source in use isn't closed by a purge.
        let busy = pool.inner.source(&Arc::from(files[0].path.as_str()));
        FootageSource::purge(&pool);
        assert_eq!(pool.open_files(), 1);
        drop(busy);
        FootageSource::purge(&pool);
        assert_eq!(pool.open_files(), 0);
    }

    /// A Photoshop document imported as a composition: every layer item decodes from one parsed
    /// document (each re-read and re-parsed the whole file), with the same pixels; parsed
    /// documents are bounded and forgotten with their file.
    #[test]
    fn psd_layers_share_one_parsed_document() {
        use effectcraft_psd::Rect;
        use effectcraft_psd::write::{WDoc, WLayer};
        let pool = MediaPool::new();
        let mut doc = WDoc::new(32, 16);
        doc.layers =
            vec![WLayer::solid("Back", Rect::new(0, 0, 32, 16), [0.2, 0.3, 0.4, 1.0]), WLayer::solid("Red", Rect::new(4, 2, 10, 8), [1.0, 0.0, 0.0, 1.0])];
        let bytes: Arc<[u8]> = effectcraft_psd::write::write(&doc).into();
        let psd_footage = |path: &str, index: Option<u32>| Footage {
            path: path.into(),
            kind: FootageKind::Still,
            width: 32,
            height: 16,
            has_video: true,
            alpha: AlphaMode::Straight,
            layer: index.map(|index| effectcraft_project::SourceLayer { index, name: String::new(), ..Default::default() }),
            ..Default::default()
        };
        pool.add_bytes("/doc.psd", bytes.clone());
        for index in [None, Some(0), Some(1)] {
            let f = psd_footage("/doc.psd", index);
            let got = pool.frame_at(&f, Tick::ZERO).unwrap();
            let fresh = crate::layered::decode_psd("/doc.psd", &crate::layered::parse_psd("/doc.psd", &bytes).unwrap(), &f, AlphaOp::of(&f)).unwrap();
            assert_eq!(*got, fresh, "layer {index:?}");
        }
        assert_eq!(lock(&pool.inner.psds).list.len(), 1, "parsed once");
        pool.forget("/doc.psd");
        assert!(lock(&pool.inner.psds).list.is_empty());
        for i in 0..MAX_PSDS + 2 {
            let path = format!("/doc{i}.psd");
            pool.add_bytes(&path, bytes.clone());
            pool.frame_at(&psd_footage(&path, Some(1)), Tick::ZERO).unwrap();
        }
        let psds = lock(&pool.inner.psds);
        assert_eq!(psds.list.len(), MAX_PSDS);
        assert_eq!(&*psds.list[MAX_PSDS - 1].0, format!("/doc{}.psd", MAX_PSDS + 1));
    }

    /// #324: audio that fails to decode isn't conformed. A conformed file of silence was written
    /// instead, and as it is found by the file's path, size and date the footage stayed silent
    /// in every later session (a renamed copy played).
    #[test]
    fn audio_that_fails_to_decode_is_not_conformed() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/test-conform-fail");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("song.mp3");
        std::fs::write(&path, b"ID3, not really").unwrap();
        let footage = Footage {
            path: path.to_string_lossy().into(),
            kind: FootageKind::Audio,
            duration: Tick::from_seconds_f64(2.0),
            has_audio: true,
            ..Default::default()
        };
        let pool = MediaPool::new();
        let conformed = dir.join("conformed");
        pool.set_conform_folder(Some(conformed.clone()));
        let file = pool.conformed_path(&footage.path, 48_000).unwrap();
        assert!(pool.audio_samples(&footage, Tick::ZERO, 1024, 48_000).iter().all(|v| *v == 0.0));
        for _ in 0..500 {
            if lock(&pool.inner.conforming).is_empty() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(lock(&pool.inner.conforming).is_empty(), "the conform thread finished");
        assert!(!file.exists() && !file.with_extension("tmp").exists(), "no conformed file was left");
        assert!(lock(&pool.inner.conform_failed).contains(&file));
        // Later reads go to the decoder without trying again.
        pool.audio_samples(&footage, Tick::ZERO, 1024, 48_000);
        assert!(lock(&pool.inner.conforming).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An intra-only (ProRes) movie of `n` frames, each a different gradient.
    fn prores_movie(w: u32, h: u32, n: u32) -> Vec<u8> {
        use filmcraft_isobmff::{Brand, FourCc, Mp4Writer, SampleEntry, TrackConfig, WriteSample, WriterOptions};
        use filmcraft_prores::{ChromaFormat, Encoder, Frame, Profile};
        let mut enc = Encoder::new(Profile::Standard, w, h);
        let mut mux = Mp4Writer::new(std::io::Cursor::new(Vec::new()), WriterOptions::new(Brand::Mov)).unwrap();
        let vt = mux.add_track(TrackConfig::new(SampleEntry::prores(FourCc(*b"apcn"), w as u16, h as u16), 24)).unwrap();
        for i in 0..n {
            let mut fr = Frame::new(w, h, ChromaFormat::Yuv422, 10, false);
            for (k, y) in fr.y.iter_mut().enumerate() {
                *y = 64 + ((k as u32 % w) * 7 + (k as u32 / w) * 3 + i * 40) as u16 % 876;
            }
            for (k, c) in fr.cb.iter_mut().enumerate() {
                *c = 64 + (k as u32 * 5 + i * 60) as u16 % 896;
            }
            let data = enc.encode(&fr).unwrap();
            mux.write_sample(vt, WriteSample { data: &data, duration: 1, composition_offset: 0, is_sync: true }).unwrap();
        }
        mux.finish().unwrap().into_inner()
    }

    /// Decode-ahead of an intra-only movie decodes several frames at once and caches them in
    /// frame order, with the same pixels as decoding each frame on its own.
    #[test]
    fn intra_decode_ahead_matches_serial_decode() {
        let n = 24;
        let bytes: Arc<[u8]> = prores_movie(64, 48, n).into();
        let path = "/mem/intra.mov";
        let footage = crate::probe::probe_bytes(path, bytes.clone()).unwrap();
        assert!(footage.codec.contains("ProRes"), "{}", footage.codec);
        assert_eq!(ahead_batch(&footage.codec), (std::thread::available_parallelism().map_or(1, |n| n.get()) / 2).clamp(1, 4));
        assert_eq!(ahead_batch("H.264 High"), 1);
        let at = |i: u32| footage.frame_rate.tick_of(i64::from(i)) + Tick(footage.frame_rate.frame_duration().0 / 2);

        // serial: every frame decoded on its own, no decode-ahead
        let serial = MediaPool::new();
        serial.set_read_ahead(false);
        serial.add_bytes(path, bytes.clone());
        let want: Vec<Arc<Image>> = (0..n).map(|i| serial.inner.frame_at(&footage, at(i), true).unwrap()).collect();
        assert!(want.windows(2).all(|p| p[0].data != p[1].data), "the frames differ");

        // two reads in order start the decode-ahead of the frames after them
        let pool = MediaPool::new();
        pool.add_bytes(path, bytes);
        for i in 0..2 {
            assert_eq!(pool.inner.frame_at(&footage, at(i), true).unwrap().data, want[i as usize].data);
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        loop {
            let c = lock(&pool.inner.cache);
            if c.ahead.get(path).is_some_and(|a| a.runner.is_none()) {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "decode-ahead finishes");
            drop(pool.inner.done.wait_timeout(c, std::time::Duration::from_millis(20)));
        }
        let ahead = pool.stats().prefetched;
        assert!(ahead >= 16, "decoded ahead: {ahead}");
        // cached in frame order (each newer than the one before), and identical to serial decode
        let c = lock(&pool.inner.cache);
        let stamps: Vec<u64> =
            (2..2 + ahead as u32).map(|i| c.lru.map.get(&Key { frame: i64::from(i), ..Inner::locate(&footage, at(i)).key }).unwrap().stamp).collect();
        assert!(stamps.windows(2).all(|p| p[0] < p[1]), "in frame order: {stamps:?}");
        drop(c);
        for i in 2..n {
            assert_eq!(pool.inner.frame_at(&footage, at(i), true).unwrap().data, want[i as usize].data, "frame {i}");
        }
    }

    /// A6: after a backward jump (or a loop wrap) the decode-ahead window starts over from the
    /// new position: it kept its old far end, so the runner decoded everything in between (the
    /// whole clip on a wrap), past [`MAX_AHEAD`] and the memory budget.
    #[test]
    fn decode_ahead_window_shrinks_after_a_backward_jump() {
        let n = 160;
        let bytes: Arc<[u8]> = prores_movie(16, 16, n).into();
        let path = "/mem/jump.mov";
        let footage = crate::probe::probe_bytes(path, bytes.clone()).unwrap();
        let at = |i: u32| footage.frame_rate.tick_of(i64::from(i)) + Tick(footage.frame_rate.frame_duration().0 / 2);
        let pool = MediaPool::new();
        pool.add_bytes(path, bytes);
        let until = |pool: &MediaPool| lock(&pool.inner.cache).ahead.get(path).map(|a| a.until);
        for i in [120, 121] {
            pool.inner.frame_at(&footage, at(i), true).unwrap();
        }
        assert!(until(&pool).is_some_and(|u| u > 121), "decoding ahead of frame 121: {:?}", until(&pool));
        // a loop wrap / backward jump to the start, then reading on in order
        for i in [2, 3] {
            pool.inner.frame_at(&footage, at(i), true).unwrap();
        }
        let u = until(&pool).unwrap();
        assert!(u <= 4 + MAX_AHEAD, "the window ends at {u}, not near frame 3");
    }

    #[test]
    fn frame_index_loops_and_clamps() {
        let r = FrameRate::FPS_30;
        assert_eq!(Inner::frame_index(r, r.tick_of(-3), 10, 1), 0);
        assert_eq!(Inner::frame_index(r, r.tick_of(4), 10, 1), 4);
        assert_eq!(Inner::frame_index(r, r.tick_of(25), 10, 1), 9);
        assert_eq!(Inner::frame_index(r, r.tick_of(25), 10, 3), 5);
        assert_eq!(Inner::frame_index(r, r.tick_of(45), 10, 3), 9);
        assert_eq!(Inner::frame_count(r, Tick(2 * TICKS_PER_SECOND)), 60);
    }

    #[test]
    fn saved_vector_crop_has_distinct_media_cache_key() {
        let mut footage = Footage {
            path: "art.ai".into(),
            kind: FootageKind::Still,
            layer: Some(effectcraft_project::SourceLayer { index: 1, name: "Art".into(), crop_bounds: Some([0.0, 0.0, 20.0, 40.0]), ..Default::default() }),
            ..Default::default()
        };
        let before = Inner::locate(&footage, Tick::ZERO).key;
        footage.layer.as_mut().unwrap().crop_bounds = Some([1.0, 0.0, 21.0, 40.0]);
        assert!(before != Inner::locate(&footage, Tick::ZERO).key);
    }

    #[test]
    fn lru_respects_budget() {
        let mut c = Lru::default();
        let img = || Pixels::Float(Arc::new(Image::new(16, 16))); // 4 KiB + overhead
        let k = |i| Key { path: "a".into(), frame: i, interp: 0, matte: [0; 3], crop: None, preview: None, raster: 0 };
        let budget = 3 * (4096 + 64);
        for i in 0..10 {
            c.insert(k(i), img(), budget);
        }
        assert_eq!(c.map.len(), 3);
        assert!(c.get(&k(9)).is_some() && c.get(&k(6)).is_none());
        // touching 7 makes 8 the oldest
        c.get(&k(7));
        c.insert(k(10), img(), budget);
        assert!(c.get(&k(7)).is_some() && c.get(&k(8)).is_none());
    }

    /// #482: an OpenEXR file with HTJ2K compression (which the decoder lacks) says so on import
    /// and when its frame is read, instead of "no non-deep rgb channels" or a transparent frame.
    #[test]
    fn htj2k_exr_is_reported_as_unsupported() {
        use exr::prelude::*;
        let channels =
            AnyChannels::sort(vec![AnyChannel::new("R", FlatSamples::F32(vec![0.5; 4])), AnyChannel::new("G", FlatSamples::F32(vec![0.5; 4]))].into());
        let image = exr::image::Image::from_layer(Layer::new((2, 2), LayerAttributes::default(), Encoding::UNCOMPRESSED, channels));
        let mut bytes = std::io::Cursor::new(Vec::new());
        image.write().to_buffered(&mut bytes).unwrap();
        let mut bytes = bytes.into_inner();
        assert_eq!(crate::exr_channels::unsupported_compression(&bytes), None);
        // The header's compression attribute (name, type, size 1, value) says HTJ2K32 (11).
        let tag = b"compression\0compression\0\x01\0\0\0";
        let at = bytes.windows(tag.len()).position(|w| w == tag).unwrap() + tag.len();
        bytes[at] = 11;
        let b: Arc<[u8]> = bytes.into();
        let e = crate::probe_bytes("/shot.exr", b.clone()).unwrap_err().to_string();
        assert!(e.contains("/shot.exr: OpenEXR HTJ2K compression isn't supported yet"), "{e}");
        let pool = MediaPool::new();
        pool.add_bytes("/shot.exr", b);
        let f = Footage { path: "/shot.exr".into(), kind: FootageKind::Still, width: 2, height: 2, has_video: true, ..Default::default() };
        let e = pool.frame_at(&f, Tick::ZERO).unwrap_err().to_string();
        assert!(e.contains("HTJ2K compression isn't supported yet"), "{e}");
    }

    /// #411: Interpret Footage ▸ Preserve RGB reads a float OpenEXR's values as the file stores
    /// them (no automatic linear → sRGB encode), so an OCIO effect is the only conversion; the
    /// default interpretation still encodes. Both interpretations of one file decode separately.
    #[test]
    fn preserve_rgb_reads_float_exr_values_unconverted() {
        use exr::prelude::*;
        let (w, h) = (2usize, 2usize);
        let ch = |n: &str, v: f32| AnyChannel::new(n, FlatSamples::F32(vec![v; w * h]));
        let channels = AnyChannels::sort(vec![ch("R", 0.18), ch("G", 0.5), ch("B", 2.0), ch("A", 1.0)].into());
        let image = exr::image::Image::from_layer(Layer::new((w, h), LayerAttributes::default(), Encoding::FAST_LOSSLESS, channels));
        let mut bytes = std::io::Cursor::new(Vec::new());
        image.write().to_buffered(&mut bytes).unwrap();
        let b: Arc<[u8]> = bytes.into_inner().into();
        let pool = MediaPool::new();
        pool.add_bytes("/render.exr", b.clone());
        let f = crate::probe_bytes("/render.exr", b).unwrap();
        let encoded = pool.frame_at(&f, Tick::ZERO).unwrap().get(0, 0);
        assert!((encoded[0] - 0.4613).abs() < 1e-3 && (encoded[1] - 0.7354).abs() < 1e-3 && encoded[2] > 1.3, "sRGB-encoded: {encoded:?}");
        let raw = pool.frame_at(&Footage { preserve_rgb: true, ..f.clone() }, Tick::ZERO).unwrap().get(0, 0);
        assert_eq!(raw, [0.18, 0.5, 2.0, 1.0]);
        assert_eq!(pool.frame_at(&f, Tick::ZERO).unwrap().get(0, 0), encoded, "the default interpretation is cached apart");
    }
}
