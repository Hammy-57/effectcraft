//! Layer render cache.
//!
//! The processed pixels of a layer (source → masks → effects, in layer space) depend only on the
//! layer's own non-transform properties, its source item and the render options — not on its
//! transform, opacity, blend mode or anything below it. [`LayerCache`] keeps those buffers keyed
//! by a content hash of exactly those inputs, *evaluated at the frame time*, so:
//!
//! * a static layer (or one whose only animation is in Transform) renders once and is reused on
//!   every frame while scrubbing or playing;
//! * editing one layer re-renders only that layer — every other layer's cached pixels stay valid
//!   because their keys do not change;
//! * any change to a property value, keyframe, expression result, effect, mask, switch or source
//!   item changes the key, so stale pixels are never returned.
//!
//! Layers whose pixels depend on time in ways the property values do not capture (time-based
//! effects such as Noise or Wave Warp, Wiggle Paths, moving footage) fold the layer time (or the
//! source frame) into the key; a still is keyed by its item alone. A precomp is keyed by its
//! nested comp's content at the nested time ([`crate::precomp_key`]), so a static precomp
//! renders once.
//!
//! The static part of a key (the property tree's structure, formatted and hashed) is memoised
//! per shared layer ([`LayerInfo`]): layers are `Arc`-shared between project snapshots and an
//! edit makes a new allocation, so an allocation that is still alive still holds the same
//! layer. The memo keeps a `Weak` on each allocation it describes, so the address can't be
//! reused by another layer while the entry exists.
//!
//! Concurrent misses on one key (parallel frames reaching the same static layer) render it once:
//! the first claims the key ([`LayerCache::claim`]) and the others wait for its buffer.
//!
//! **Admission.** Most of what an animated comp renders is used once (a layer whose effect
//! animates has a new key every frame), and keeping it would fill the budget with buffers that
//! never hit. So a buffer whose key was never rendered before goes on probation: a small window
//! ([`LayerCache::window`], first in first out) that holds the most recent first renders. A hit
//! there (the next frame of a static layer, a precomp used again in the same frame, a waiter of
//! the in-flight render) admits it to the cache proper. A key rendered again after its probation
//! entry left the window (a busy frame flushed it, a RAM-preview loop came round) is remembered
//! by a bounded doorkeeper of recently rendered keys and admitted straight away.

use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Condvar, LazyLock, Mutex, PoisonError, RwLock, Weak};

use effectcraft_effects::Buf;
use effectcraft_project::{Comp, FootageKind, ItemKind, Layer, LayerSource, Node, PropGroup};

use crate::eval::EvalCtx;
use crate::packed::PackedBuf;

/// Thread-safe, memory-budgeted cache of processed layer buffers, optionally backed by the
/// persistent [`DiskCache`](crate::disk_cache::DiskCache): a memory miss reads the disk, and
/// buffers that were slow to render are written there (keys salted per project, see
/// [`LayerCache::set_disk`]).
pub struct LayerCache {
    inner: Mutex<Inner>,
    disk: Mutex<Option<(Arc<crate::disk_cache::DiskCache>, u64)>>,
    /// Another persistent store when there is no disk cache (the browser's, [`PrefetchStore`]).
    store: Mutex<Option<(Arc<dyn LayerStore>, u64)>>,
    /// While set, inserts are dropped (see [`LayerCache::set_gate`]).
    gate: Mutex<Option<Arc<std::sync::atomic::AtomicBool>>>,
    /// Keys being rendered right now, by whom (see [`LayerCache::claim`]).
    inflight: Mutex<HashMap<u64, Arc<Flight>>>,
    /// Precomp layers' consecutive key misses (see [`LayerCache::precomp_try`]).
    streaks: Mutex<HashMap<u64, u32>>,
}

struct Entry {
    buf: Stored,
    /// Bytes of pixel storage (packed size when packed).
    bytes: usize,
    last_use: u64,
    /// [`PackedBuf::pack`] declined this buffer (not on an integer grid): don't try again.
    unpackable: bool,
    /// A first render on probation (see the module docs): admitted on its first hit, dropped
    /// first in first out when the probation window is full.
    probation: bool,
}

/// How an entry keeps its pixels. Entries go in as `f32` buffers (a hit shares the `Arc`, and
/// the GPU compositor keeps their textures while it lives); only when the cache is over budget
/// are the least recently used ones packed (lossless, see [`crate::packed`]) before anything
/// is evicted, and a hit on a packed entry unpacks it back to `f32`.
enum Stored {
    Full(Arc<Buf>),
    Packed(Arc<PackedBuf>),
}

struct Inner {
    map: HashMap<u64, Entry>,
    bytes: usize,
    budget: usize,
    clock: u64,
    hits: u64,
    misses: u64,
    /// Buffers inserted (rendered, or read from disk).
    inserts: u64,
    /// When each recent miss happened (to time the render that follows it).
    missed_at: HashMap<u64, web_time::Instant>,
    /// Bytes of the entries on probation (part of `bytes`).
    probation_bytes: usize,
    /// Keys rendered recently (admission, see the module docs).
    seen: Doorkeeper,
}

/// Keys per doorkeeper generation: two generations are kept, so the last 16–32k rendered keys
/// are remembered (a few hundred KB).
const DOOR_GEN: usize = 16 * 1024;

/// A bounded set of recently rendered keys: two generations, the older dropped when the newer
/// fills (a cheap approximation of least recently seen).
#[derive(Default)]
struct Doorkeeper {
    cur: HashSet<u64>,
    old: HashSet<u64>,
}

impl Doorkeeper {
    /// Note a render of `key`; whether it was rendered before.
    fn saw(&mut self, key: u64) -> bool {
        if self.cur.contains(&key) {
            return true;
        }
        let before = self.old.contains(&key);
        if self.cur.len() >= DOOR_GEN {
            self.old = std::mem::take(&mut self.cur);
        }
        self.cur.insert(key);
        before
    }

    fn clear(&mut self) {
        self.cur.clear();
        self.old.clear();
    }
}

/// Hit/miss counters and memory use (for perf readouts and tests).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CacheStats {
    pub entries: usize,
    pub bytes: usize,
    pub hits: u64,
    pub misses: u64,
}

impl Default for LayerCache {
    fn default() -> Self {
        LayerCache::new(1 << 30)
    }
}

impl LayerCache {
    /// A cache holding at most `budget` bytes of pixels.
    pub fn new(budget: usize) -> LayerCache {
        LayerCache {
            inner: Mutex::new(Inner {
                map: HashMap::new(),
                bytes: 0,
                budget,
                clock: 0,
                hits: 0,
                misses: 0,
                inserts: 0,
                missed_at: HashMap::new(),
                probation_bytes: 0,
                seen: Doorkeeper::default(),
            }),
            disk: Mutex::new(None),
            store: Mutex::new(None),
            gate: Mutex::new(None),
            inflight: Mutex::new(HashMap::new()),
            streaks: Mutex::new(HashMap::new()),
        }
    }

    /// Drop inserts while `gate` is set. A render whose GPU readbacks are still in flight (the
    /// browser's deferred readbacks, see [`crate::Accelerator::pass_missed`]) computes some
    /// buffers from placeholders; the accelerator raises the gate from its first missing
    /// readback to the end of the pass, so none of them is kept (in memory or on disk).
    pub fn set_gate(&self, gate: Option<Arc<std::sync::atomic::AtomicBool>>) {
        if let Ok(mut g) = self.gate.lock() {
            *g = gate;
        }
    }

    fn gated(&self) -> bool {
        self.gate.lock().ok().and_then(|g| g.as_ref().map(|g| g.load(std::sync::atomic::Ordering::Relaxed))).unwrap_or(false)
    }

    /// Attach (or detach) the disk cache. `salt` separates projects and footage versions (see
    /// `footage_salt`); keys on disk are `(salt, key)`.
    pub fn set_disk(&self, disk: Option<(Arc<crate::disk_cache::DiskCache>, u64)>) {
        if let Ok(mut d) = self.disk.lock() {
            *d = disk;
        }
    }

    /// Change the disk salt (after footage changed) keeping the attached disk cache.
    pub fn set_disk_salt(&self, salt: u64) {
        if let Ok(mut d) = self.disk.lock()
            && let Some((_, s)) = d.as_mut()
        {
            *s = salt;
        }
    }

    pub fn disk(&self) -> Option<Arc<crate::disk_cache::DiskCache>> {
        self.disk.lock().ok()?.as_ref().map(|d| d.0.clone())
    }

    /// Attach (or detach) a [`LayerStore`] used when no disk cache is attached (the browser's
    /// Origin Private File System through a [`PrefetchStore`]); `salt` as in
    /// [`LayerCache::set_disk`].
    pub fn set_store(&self, store: Option<(Arc<dyn LayerStore>, u64)>) {
        if let Ok(mut s) = self.store.lock() {
            *s = store;
        }
    }

    /// Change the store's salt (a new project or footage), keeping the store.
    pub fn set_store_salt(&self, salt: u64) {
        if let Ok(mut d) = self.store.lock()
            && let Some((_, s)) = d.as_mut()
        {
            *s = salt;
        }
    }

    fn disk_key(&self, key: u64) -> Option<(Arc<dyn LayerStore>, u128)> {
        let salted = |salt: u64| ((salt as u128) << 64) | key as u128;
        if let Some((dc, salt)) = self.disk.lock().ok()?.as_ref() {
            return Some((dc.clone() as Arc<dyn LayerStore>, salted(*salt)));
        }
        let s = self.store.lock().ok()?;
        let (st, salt) = s.as_ref()?;
        Some((st.clone(), salted(*salt)))
    }

    pub fn get(&self, key: u64) -> Option<Arc<Buf>> {
        if let Some(b) = self.peek(key) {
            return Some(b);
        }
        {
            let mut g = self.inner.lock().ok()?;
            g.misses += 1;
            if g.missed_at.len() > 4096 {
                g.missed_at.clear();
            }
            g.missed_at.insert(key, web_time::Instant::now());
        }
        // Disk cache: a hit comes back into memory.
        let (dc, dk) = self.disk_key(key)?;
        let buf = dc.get_layer(dk)?;
        // (Stored on disk: it was worth keeping.)
        self.insert_mem(key, buf.clone(), true);
        Some(buf)
    }

    /// [`Self::get`] for a buffer that is only worth having from memory: no disk lookup, and a
    /// miss is not counted (the caller falls back to a path that looks it up properly).
    pub(crate) fn peek(&self, key: u64) -> Option<Arc<Buf>> {
        let mut g = self.inner.lock().ok()?;
        g.clock += 1;
        let now = g.clock;
        let e = g.map.get_mut(&key)?;
        e.last_use = now;
        let stored = match &e.buf {
            Stored::Full(b) => Ok(b.clone()),
            Stored::Packed(p) => Err(p.clone()),
        };
        // Used again: admitted.
        let admitted = std::mem::take(&mut e.probation).then_some(e.bytes);
        if let Some(b) = admitted {
            g.probation_bytes = g.probation_bytes.saturating_sub(b);
        }
        g.hits += 1;
        drop(g);
        Some(match stored {
            Ok(b) => b,
            Err(p) => self.unpacked(key, &p),
        })
    }

    pub fn insert(&self, key: u64, buf: Arc<Buf>) {
        if self.gated() {
            return;
        }
        let (started, again) = match self.inner.lock() {
            Ok(mut g) => (g.missed_at.remove(&key), g.seen.saw(key)),
            Err(_) => return,
        };
        if let Some((dc, dk)) = self.disk_key(key) {
            let slow = started.is_some_and(|t| t.elapsed().as_secs_f64() * 1e3 >= dc.min_layer_ms() as f64);
            if slow {
                dc.put_layer(dk, &buf);
            }
        }
        self.insert_mem(key, buf, again);
    }

    /// Keep `buf` under `key`: in the cache proper when `admit`, else on probation (see the
    /// module docs).
    fn insert_mem(&self, key: u64, buf: Arc<Buf>, admit: bool) {
        let bytes = buf.img.data.len() * std::mem::size_of::<effectcraft_raster::Px>();
        {
            let Ok(mut g) = self.inner.lock() else { return };
            if bytes > g.budget / 4 {
                return;
            }
            g.clock += 1;
            g.inserts += 1;
            let now = g.clock;
            // An entry already admitted stays admitted.
            let probation = !admit && g.map.get(&key).is_none_or(|e| e.probation);
            if let Some(old) = g.map.insert(key, Entry { buf: Stored::Full(buf), bytes, last_use: now, unpackable: false, probation }) {
                g.bytes -= old.bytes;
                if old.probation {
                    g.probation_bytes = g.probation_bytes.saturating_sub(old.bytes);
                }
            }
            g.bytes += bytes;
            if probation {
                g.probation_bytes += bytes;
                trim_probation(&mut g, key);
            }
        }
        self.relieve();
    }

    /// Bytes of first renders kept on probation, at most (the newest is kept whatever its size,
    /// so a big precomp used twice in a row still hits): 1/16 of the budget, at most 128 MB.
    pub fn window(&self) -> usize {
        self.inner.lock().map(|g| window_of(g.budget)).unwrap_or(0)
    }

    /// A packed entry's pixels as `f32`: unpacked (outside the lock) and kept unpacked, as it is
    /// in use again.
    fn unpacked(&self, key: u64, p: &Arc<PackedBuf>) -> Arc<Buf> {
        let buf = Arc::new(p.unpack());
        let bytes = buf.img.data.len() * std::mem::size_of::<effectcraft_raster::Px>();
        {
            let Ok(mut g) = self.inner.lock() else { return buf };
            let Some(e) = g.map.get_mut(&key) else { return buf };
            match &e.buf {
                // Another thread unpacked it first: share its buffer.
                Stored::Full(b) => return b.clone(),
                Stored::Packed(q) if !Arc::ptr_eq(p, q) => return buf,
                Stored::Packed(_) => {}
            }
            let old = std::mem::replace(&mut e.bytes, bytes);
            e.buf = Stored::Full(buf.clone());
            g.bytes = g.bytes.saturating_sub(old) + bytes;
        }
        self.relieve();
        buf
    }

    /// Over budget: pack the least recently used `f32` entries (outside the lock), then evict
    /// least recently used entries down to 3/4 of the budget if that wasn't enough.
    fn relieve(&self) {
        let picked: Vec<(u64, Arc<Buf>)> = {
            let Ok(mut g) = self.inner.lock() else { return };
            if g.bytes <= g.budget {
                return;
            }
            let excess = g.bytes - g.budget / 4 * 3;
            let mut by_age: Vec<(u64, u64)> =
                g.map.iter().filter(|(_, e)| matches!(e.buf, Stored::Full(_)) && !e.unpackable && !e.probation).map(|(k, e)| (e.last_use, *k)).collect();
            by_age.sort_unstable();
            // Packing saves at least half (16 bpc).
            let mut saved = 0;
            let mut picked = vec![];
            for (_, k) in by_age {
                if saved >= excess {
                    break;
                }
                if let Some(e) = g.map.get_mut(&k)
                    && let Stored::Full(b) = &e.buf
                {
                    saved += e.bytes / 2;
                    picked.push((k, b.clone()));
                    // Not picked again by a concurrent call while this one packs it.
                    e.unpackable = true;
                }
            }
            picked
        };
        let packed: Vec<_> = picked.into_iter().map(|(k, b)| (k, PackedBuf::pack(&b), b)).collect();
        let Ok(mut g) = self.inner.lock() else { return };
        for (k, p, b) in packed {
            let Some(e) = g.map.get_mut(&k) else { continue };
            if !matches!(&e.buf, Stored::Full(cur) if Arc::ptr_eq(cur, &b)) {
                continue;
            }
            let Some(p) = p else { continue };
            let bytes = p.bytes();
            let old = std::mem::replace(&mut e.bytes, bytes);
            e.buf = Stored::Packed(Arc::new(p));
            e.unpackable = false;
            g.bytes = g.bytes.saturating_sub(old) + bytes;
        }
        if g.bytes > g.budget {
            let target = g.budget / 4 * 3;
            evict_lru(&mut g, target);
        }
    }

    /// The memory budget in bytes.
    pub fn budget(&self) -> usize {
        self.inner.lock().map(|g| g.budget).unwrap_or(0)
    }

    /// Change the memory budget (evicts least-recently used entries when shrinking).
    pub fn set_budget(&self, budget: usize) {
        let Ok(mut g) = self.inner.lock() else { return };
        g.budget = budget;
        if g.bytes > budget {
            evict_lru(&mut g, budget);
        }
    }

    pub fn clear(&self) {
        if let Ok(mut g) = self.inner.lock() {
            g.map.clear();
            g.bytes = 0;
            g.probation_bytes = 0;
            g.seen.clear();
        }
    }

    pub fn stats(&self) -> CacheStats {
        self.inner.lock().map(|g| CacheStats { entries: g.map.len(), bytes: g.bytes, hits: g.hits, misses: g.misses }).unwrap_or_default()
    }

    /// Buffers inserted so far (each a render, or a disk read): tests count renders with it.
    #[cfg(test)]
    pub(crate) fn inserts(&self) -> u64 {
        self.inner.lock().map(|g| g.inserts).unwrap_or(0)
    }

    /// (Entries admitted, entries on probation, bytes on probation): tests check admission.
    #[cfg(test)]
    pub(crate) fn admission(&self) -> (usize, usize, usize) {
        self.inner
            .lock()
            .map(|g| {
                let probation = g.map.values().filter(|e| e.probation).count();
                (g.map.len() - probation, probation, g.probation_bytes)
            })
            .unwrap_or_default()
    }

    /// After a miss on `key`: render it ([`Claim::Owner`], whose drop wakes the waiters), take
    /// the buffer another thread just rendered under it ([`Claim::Ready`]), or render it anyway
    /// ([`Claim::Render`]: this thread is already rendering it further up its stack, or the
    /// other render didn't leave a buffer in time).
    ///
    /// A waiter on a rayon worker waits at most [`WAIT_POOL`]: rayon may run a frame that waits
    /// here on top of a stolen piece of the very render it waits for (a join inside it), which
    /// would never finish; the timeout turns that into a duplicate render, as before.
    pub(crate) fn claim(&self, key: u64) -> Claim<'_> {
        let me = std::thread::current().id();
        let flight = {
            let mut m = self.inflight.lock().unwrap_or_else(PoisonError::into_inner);
            match m.get(&key) {
                Some(f) if f.owner == me => return Claim::Render,
                Some(f) => f.clone(),
                None => {
                    let f = Arc::new(Flight { owner: me, done: Mutex::new(false), cv: Condvar::new() });
                    m.insert(key, f.clone());
                    return Claim::Owner(FlightGuard { cache: self, key, flight: f });
                }
            }
        };
        if !flight.wait() {
            return Claim::Render;
        }
        match self.peek(key) {
            Some(b) => Claim::Ready(b),
            None => Claim::Render,
        }
    }

    /// Whether to key precomp layer `id` (a hash of its comp and layer) this time. A precomp
    /// whose key missed on several frames in a row is animated: keying it evaluates its nested
    /// comp's animated properties (expressions included) once more than rendering it does, so
    /// it is keyed only every eighth frame until a key hits again.
    pub(crate) fn precomp_try(&self, id: u64) -> bool {
        let Ok(mut m) = self.streaks.lock() else { return true };
        let n = m.get(&id).copied().unwrap_or(0);
        let attempt = n < 2 || n % 8 == 0;
        if !attempt {
            if m.len() > 4096 {
                m.clear();
            }
            m.insert(id, n.saturating_add(1));
        }
        attempt
    }

    /// The outcome of keying precomp layer `id` (see [`Self::precomp_try`]).
    pub(crate) fn precomp_note(&self, id: u64, hit: bool) {
        let Ok(mut m) = self.streaks.lock() else { return };
        if m.len() > 4096 {
            m.clear();
        }
        let n = m.entry(id).or_insert(0);
        *n = if hit { 0 } else { n.saturating_add(1) };
    }
}

/// How long a waiter on a rayon worker waits for another thread's render (see
/// [`LayerCache::claim`]).
const WAIT_POOL: std::time::Duration = std::time::Duration::from_secs(2);
/// How long any other thread waits.
const WAIT_OTHER: std::time::Duration = std::time::Duration::from_secs(30);

/// A key being rendered.
struct Flight {
    owner: std::thread::ThreadId,
    done: Mutex<bool>,
    cv: Condvar,
}

impl Flight {
    /// Wait until the render is done (`true`) or the time is up (`false`).
    fn wait(&self) -> bool {
        // Single-threaded targets never get here (the owner is this thread, see `claim`).
        if cfg!(target_arch = "wasm32") {
            return false;
        }
        let limit = if rayon::current_thread_index().is_some() { WAIT_POOL } else { WAIT_OTHER };
        let start = web_time::Instant::now();
        let mut done = self.done.lock().unwrap_or_else(PoisonError::into_inner);
        while !*done {
            let Some(left) = limit.checked_sub(start.elapsed()) else { return false };
            done = match self.cv.wait_timeout(done, left) {
                Ok((g, _)) => g,
                Err(e) => e.into_inner().0,
            };
        }
        true
    }
}

/// The outcome of [`LayerCache::claim`].
pub(crate) enum Claim<'c> {
    /// Render the buffer and insert it; dropping the guard (after the insert, or on failure)
    /// wakes the waiters.
    Owner(#[allow(dead_code)] FlightGuard<'c>),
    /// Another thread rendered it.
    Ready(Arc<Buf>),
    /// Render it without waking anyone.
    Render,
}

/// Ends a [`LayerCache::claim`] when dropped (also when the render panics).
pub(crate) struct FlightGuard<'c> {
    cache: &'c LayerCache,
    key: u64,
    flight: Arc<Flight>,
}

impl Drop for FlightGuard<'_> {
    fn drop(&mut self) {
        {
            let mut m = self.cache.inflight.lock().unwrap_or_else(PoisonError::into_inner);
            if m.get(&self.key).is_some_and(|f| Arc::ptr_eq(f, &self.flight)) {
                m.remove(&self.key);
            }
        }
        *self.flight.done.lock().unwrap_or_else(PoisonError::into_inner) = true;
        self.flight.cv.notify_all();
    }
}

/// Evict least recently used entries until at most `target` bytes are kept.
fn evict_lru(g: &mut Inner, target: usize) {
    let mut by_age: Vec<(u64, u64)> = g.map.iter().map(|(k, e)| (e.last_use, *k)).collect();
    by_age.sort_unstable();
    for (_, k) in by_age {
        if g.bytes <= target {
            break;
        }
        remove(g, k);
    }
}

fn remove(g: &mut Inner, key: u64) {
    if let Some(e) = g.map.remove(&key) {
        g.bytes -= e.bytes;
        if e.probation {
            g.probation_bytes = g.probation_bytes.saturating_sub(e.bytes);
        }
    }
}

/// See [`LayerCache::window`].
fn window_of(budget: usize) -> usize {
    (budget / 16).min(128 << 20)
}

/// Drop the oldest entries on probation (never `newest`) until they fit the window.
fn trim_probation(g: &mut Inner, newest: u64) {
    let window = window_of(g.budget);
    if g.probation_bytes <= window {
        return;
    }
    let mut by_age: Vec<(u64, u64)> = g.map.iter().filter(|(k, e)| e.probation && **k != newest).map(|(k, e)| (e.last_use, *k)).collect();
    by_age.sort_unstable();
    for (_, k) in by_age {
        if g.probation_bytes <= window {
            break;
        }
        remove(g, k);
    }
}

/// Persistent storage of layer buffers behind a [`LayerCache`] (keys salted per project).
pub trait LayerStore: Send + Sync {
    /// A stored buffer (synchronous: mid-render).
    fn get_layer(&self, key: u128) -> Option<Arc<Buf>>;
    /// Keep a buffer that was slow to render.
    fn put_layer(&self, key: u128, buf: &Buf);
    /// Buffers faster to render than this (ms) are not stored.
    fn min_layer_ms(&self) -> u64 {
        0
    }
}

/// Layers buffers rendered slower than this go to the browser's disk cache (ms).
pub const PREFETCH_MIN_LAYER_MS: u64 = 20;

/// A [`LayerStore`] over storage that can't be read in the middle of a render (the browser's
/// Origin Private File System, read asynchronously). Lookups are served from buffers fetched
/// before the frame ([`PrefetchStore::provide`]); the keys a render looked up and didn't find
/// are recorded ([`PrefetchStore::take_misses`]) so the transport can fetch them before the next
/// frame; buffers to store queue up as sealed entries ([`PrefetchStore::take_writes`]) for the
/// transport to write after the frame. Fetched buffers are kept least recently used first, up
/// to a byte budget.
pub struct PrefetchStore {
    st: Mutex<Prefetched>,
    budget: usize,
    min_ms: u64,
}

#[derive(Default)]
struct Prefetched {
    ready: HashMap<u128, (Arc<Buf>, u64)>,
    bytes: usize,
    clock: u64,
    misses: Vec<u128>,
    hits: Vec<u128>,
    writes: Vec<(u128, Vec<u8>)>,
    /// Keys stored or queued (not written twice).
    known: std::collections::HashSet<u128>,
}

impl Default for PrefetchStore {
    fn default() -> Self {
        PrefetchStore::new(256 << 20)
    }
}

impl PrefetchStore {
    pub fn new(budget: usize) -> PrefetchStore {
        PrefetchStore { st: Mutex::new(Prefetched::default()), budget, min_ms: PREFETCH_MIN_LAYER_MS }
    }

    /// Store buffers that took at least `ms` to render (default [`PREFETCH_MIN_LAYER_MS`]).
    pub fn with_min_ms(mut self, ms: u64) -> PrefetchStore {
        self.min_ms = ms;
        self
    }

    /// Whether `key`'s buffer is here.
    pub fn has(&self, key: u128) -> bool {
        self.st.lock().is_ok_and(|s| s.ready.contains_key(&key))
    }

    /// A fetched entry (sealed bytes, [`crate::disk_cache::layer_entry`]) for `key`; `false`
    /// when damaged.
    pub fn provide(&self, key: u128, entry: &[u8]) -> bool {
        let Some(buf) = crate::disk_cache::read_layer_entry(entry) else { return false };
        let bytes = buf.img.data.len() * std::mem::size_of::<effectcraft_raster::Px>();
        let Ok(mut s) = self.st.lock() else { return false };
        s.clock += 1;
        let now = s.clock;
        if let Some((old, _)) = s.ready.insert(key, (Arc::new(buf), now)) {
            s.bytes -= old.img.data.len() * std::mem::size_of::<effectcraft_raster::Px>();
        }
        s.bytes += bytes;
        s.known.insert(key);
        while s.bytes > self.budget {
            let Some((&k, _)) = s.ready.iter().min_by_key(|(_, (_, t))| *t) else { break };
            if let Some((b, _)) = s.ready.remove(&k) {
                s.bytes -= b.img.data.len() * std::mem::size_of::<effectcraft_raster::Px>();
            }
        }
        true
    }

    /// Keys looked up and not found since the last call (each once).
    pub fn take_misses(&self) -> Vec<u128> {
        self.st.lock().map(|mut s| std::mem::take(&mut s.misses)).unwrap_or_default()
    }

    /// Keys served since the last call (each once).
    pub fn take_hits(&self) -> Vec<u128> {
        self.st.lock().map(|mut s| std::mem::take(&mut s.hits)).unwrap_or_default()
    }

    /// Entries to write: (key, sealed bytes).
    pub fn take_writes(&self) -> Vec<(u128, Vec<u8>)> {
        self.st.lock().map(|mut s| std::mem::take(&mut s.writes)).unwrap_or_default()
    }

    /// Forget everything (Empty Disk Cache, Purge).
    pub fn clear(&self) {
        if let Ok(mut s) = self.st.lock() {
            *s = Prefetched::default();
        }
    }
}

impl LayerStore for PrefetchStore {
    fn get_layer(&self, key: u128) -> Option<Arc<Buf>> {
        let mut s = self.st.lock().ok()?;
        s.clock += 1;
        let now = s.clock;
        match s.ready.get_mut(&key) {
            Some((b, t)) => {
                *t = now;
                let b = b.clone();
                if !s.hits.contains(&key) {
                    s.hits.push(key);
                }
                Some(b)
            }
            None => {
                if !s.misses.contains(&key) && s.misses.len() < 1024 {
                    s.misses.push(key);
                }
                None
            }
        }
    }

    fn put_layer(&self, key: u128, buf: &Buf) {
        let Ok(mut s) = self.st.lock() else { return };
        if s.known.insert(key) {
            s.writes.push((key, crate::disk_cache::layer_entry(buf)));
        }
    }

    fn min_layer_ms(&self) -> u64 {
        self.min_ms
    }
}

/// FNV-1a style 64-bit hasher fed by `Debug` output (exact float formatting) and raw values.
pub(crate) struct KeyHasher(u64);

impl Hasher for KeyHasher {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 ^= b as u64;
            self.0 = self.0.wrapping_mul(0x0100_0000_01b3);
        }
    }
}

impl std::fmt::Write for KeyHasher {
    fn write_str(&mut self, s: &str) -> std::fmt::Result {
        Hasher::write(self, s.as_bytes());
        Ok(())
    }
}


pub(crate) fn hash_debug(h: &mut KeyHasher, v: &impl std::fmt::Debug) {
    use std::fmt::Write;
    let _ = write!(h, "{v:?}");
    Hasher::write_u8(h, 0xff);
}

/// A [`KeyHasher`] starting from `seed`.
pub(crate) fn hasher(seed: u64) -> KeyHasher {
    KeyHasher(seed)
}

/// Time-based layer content: does any part of the source/effects read the clock directly
/// (rather than through property values)?
fn time_dependent(layer: &Layer) -> bool {
    if layer.switches.effects
        && let Some(fx) = layer.effects()
    {
        for g in fx.groups() {
            if let effectcraft_project::GroupKind::Effect { effect } = &g.kind
                && g.enabled
                && effectcraft_effects::is_time_dependent(effect)
            {
                return true;
            }
            // Clone strokes reading another layer or another time.
            if g.enabled && effectcraft_effects::paint::is_paint(g) && effectcraft_effects::paint::cache_key(g, 0.0).1 {
                return true;
            }
        }
    }
    if matches!(layer.source, LayerSource::Shape)
        && let Some(c) = layer.props.sub("contents")
    {
        return group_has(c, "wiggle");
    }
    // Wiggly selectors move with time; Expression selectors evaluate per character (their
    // property value alone doesn't capture the result).
    if matches!(layer.source, LayerSource::Text)
        && let Some(t) = layer.props.sub("text")
    {
        return group_has(t, "wigglySelector") || group_has(t, "expressionSelector");
    }
    false
}

fn group_has(g: &PropGroup, match_id: &str) -> bool {
    g.children.iter().any(|c| match c {
        Node::Group(sub) => sub.match_id == match_id || group_has(sub, match_id),
        Node::Prop(_) => false,
    })
}

/// Feed every property of `g` that can change over time (keyframes or expression) as its value
/// at the context time. The static structure is hashed separately.
pub(crate) fn hash_values(h: &mut KeyHasher, ctx: &EvalCtx, layer: &Layer, g: &PropGroup) {
    for c in &g.children {
        match c {
            Node::Prop(p) => {
                if !p.keys.is_empty() || p.has_expression() {
                    p.uid.hash(h);
                    hash_debug(h, &ctx.value(layer, p));
                }
            }
            Node::Group(sub) => hash_values(h, ctx, layer, sub),
        }
    }
}

/// Does any property of `g` have keyframes or an expression?
fn has_animation(g: &PropGroup) -> bool {
    g.children.iter().any(|c| match c {
        Node::Prop(p) => !p.keys.is_empty() || p.has_expression(),
        Node::Group(sub) => has_animation(sub),
    })
}

// ------------------------------------------------------------------------------ key memo

/// What the keys need to know about a layer that changes only when the layer does: the hashed
/// structure of its property groups and what reads the clock. Memoised per shared layer
/// ([`layer_info`]), so a frame formats no unchanged layer's property tree.
#[derive(Clone, Copy, Debug)]
pub(crate) struct LayerInfo {
    /// Layer id, source and pixel switches.
    head: u64,
    /// Static structure of the content (every top-level node but Transform and Layer Styles):
    /// values, keyframes, expressions, enabled flags, effect ids, modes.
    content: u64,
    /// The Layer Styles group's structure.
    styles: u64,
    /// Every field of the layer, every property included (a precomp's nested layers).
    pub(crate) whole: u64,
    /// A content group has keyframes or expressions.
    content_animated: bool,
    styles_animated: bool,
    /// A property anywhere has keyframes or an expression.
    pub(crate) animated: bool,
    /// A property anywhere has an expression.
    pub(crate) expressions: bool,
    /// A group other than Transform has an expression.
    content_expressions: bool,
    pub(crate) time_dependent: bool,
    pub(crate) reads_other_times: bool,
    reads_comp_scene: bool,
    /// Enabled paint strokes (their Duration spans show and hide them).
    pub(crate) paint: bool,
    masks_or_effects: bool,
    /// An enabled effect has a layer parameter naming a layer (Displacement Map, Set Matte…).
    layer_refs: bool,
}

impl LayerInfo {
    fn of(layer: &Layer) -> LayerInfo {
        let mut head = KeyHasher(0x8422_2325_cbf2_9ce4);
        layer.id.hash(&mut head);
        hash_debug(&mut head, &layer.source);
        // (Audio, Lock and Shy don't change pixels.)
        hash_debug(&mut head, &layer.switches.pixels());
        let mut content = KeyHasher(0xcbf2_9ce4_8422_2325);
        let (mut content_animated, mut content_expressions) = (false, false);
        for c in &layer.props.children {
            if let Node::Group(g) = c {
                if g.match_id == "transform" {
                    continue;
                }
                content_expressions |= has_expression(g);
                if g.match_id == effectcraft_project::styles::GROUP {
                    continue;
                }
                content_animated |= has_animation(g);
            }
            hash_debug(&mut content, c);
        }
        let mut styles = KeyHasher(0x9e37_79b9_7f4a_7c15);
        let styles_animated = layer.layer_styles().is_some_and(|g| {
            hash_debug(&mut styles, g);
            has_animation(g)
        });
        let mut whole = KeyHasher(0x7c15_9e37_79b9_7f4a);
        hash_debug(&mut whole, layer);
        let fx = layer.switches.effects.then(|| layer.effects()).flatten();
        LayerInfo {
            head: head.finish(),
            content: content.finish(),
            styles: styles.finish(),
            whole: whole.finish(),
            content_animated,
            styles_animated,
            animated: has_animation(&layer.props),
            expressions: has_expression(&layer.props),
            content_expressions,
            time_dependent: time_dependent(layer),
            reads_other_times: reads_other_times(layer),
            reads_comp_scene: reads_comp_scene(layer),
            paint: fx.is_some_and(|fx| fx.groups().any(|g| g.enabled && effectcraft_effects::paint::is_paint(g))),
            masks_or_effects: layer.masks().is_some_and(|m| !m.children.is_empty()) || layer.effects().is_some_and(|fx| !fx.children.is_empty()),
            layer_refs: fx.is_some_and(|fx| fx.groups().any(|g| g.enabled && names_layers(g))),
        }
    }
}

/// Does a property of `g` name a layer (a layer parameter set to a layer, keyframed or driven by
/// an expression)?
fn names_layers(g: &PropGroup) -> bool {
    g.children.iter().any(|c| match c {
        Node::Prop(p) => {
            matches!(p.value, effectcraft_keyframe::Value::Layer(_))
                && (matches!(p.value, effectcraft_keyframe::Value::Layer(Some(_))) || !p.keys.is_empty() || p.has_expression())
        }
        Node::Group(sub) => names_layers(sub),
    })
}

/// The layers `g`'s layer parameters name at the context time.
fn named_layers(ctx: &EvalCtx, layer: &Layer, g: &PropGroup, out: &mut Vec<u64>) {
    for c in &g.children {
        match c {
            Node::Prop(p) if matches!(p.value, effectcraft_keyframe::Value::Layer(_)) => {
                let v = if p.keys.is_empty() && !p.has_expression() { p.value.clone() } else { ctx.value(layer, p) };
                if let effectcraft_keyframe::Value::Layer(Some(id)) = v {
                    out.push(id);
                }
            }
            Node::Prop(_) => {}
            Node::Group(sub) => named_layers(ctx, layer, sub, out),
        }
    }
}

/// Fold in what `layer`'s effects read from other layers through layer parameters: each named
/// layer's structure, animated values and source at the context time, and the layers its own
/// effects name. `None` when one can't be keyed here (a precomp, an effect reading the comp
/// scene, too deep).
fn hash_layer_refs(h: &mut KeyHasher, ctx: &EvalCtx, layer: &Layer, reads_other_times: bool, depth: usize) -> Option<()> {
    if depth > 4 {
        return None;
    }
    let Some(fx) = layer.effects() else { return Some(()) };
    let mut ids = vec![];
    for g in fx.groups().filter(|g| g.enabled) {
        named_layers(ctx, layer, g, &mut ids);
    }
    for id in ids {
        id.hash(h);
        let Some(o) = ctx.layer(effectcraft_project::LayerId(id)) else { continue };
        if o.id == layer.id {
            continue;
        }
        let oi = layer_info(ctx.comp, o);
        // Read at other times, an expression may read anything then.
        if oi.reads_comp_scene || (reads_other_times && oi.expressions) {
            return None;
        }
        oi.whole.hash(h);
        if oi.animated {
            hash_values(h, ctx, o, &o.props);
        }
        match &o.source {
            LayerSource::Solid { item } => hash_debug(h, &ctx.project.item(*item).map(|i| &i.kind)),
            LayerSource::Footage { item } | LayerSource::Model { item } => {
                let it = ctx.project.item(*item)?;
                hash_debug(h, &it.kind);
                hash_debug(h, &it.proxy);
                ctx.source_time(o).0.hash(h);
            }
            LayerSource::Comp { .. } => return None,
            _ => {}
        }
        if oi.time_dependent || oi.reads_other_times || oi.paint || reads_other_times {
            ctx.time.0.hash(h);
        }
        if o.switches.effects && oi.layer_refs {
            hash_layer_refs(h, ctx, o, reads_other_times || oi.reads_other_times, depth + 1)?;
        }
    }
    Some(())
}

/// Values memoised per shared allocation (`Arc<T>`), by address. Each entry keeps a `Weak` on
/// its allocation, so while it exists the address can't be reused by anything else, and the
/// value can't change in place (`Arc::get_mut` refuses and `Arc::make_mut` moves it to a new
/// allocation while a `Weak` exists). Entries whose value was dropped are pruned as the map
/// grows.
pub(crate) struct Memo<T, V> {
    map: HashMap<usize, (Weak<T>, V)>,
    cap: usize,
}

const MEMO_MIN: usize = 1024;

impl<T, V: Clone> Memo<T, V> {
    pub(crate) fn new() -> Memo<T, V> {
        Memo { map: HashMap::new(), cap: MEMO_MIN }
    }

    /// The value for the live allocation at `addr`.
    pub(crate) fn get(&self, addr: *const T) -> Option<V> {
        let (w, v) = self.map.get(&(addr as usize))?;
        (std::ptr::eq(w.as_ptr(), addr) && w.strong_count() > 0).then(|| v.clone())
    }

    pub(crate) fn put(&mut self, arc: &Arc<T>, v: V) {
        if self.map.len() >= self.cap {
            self.map.retain(|_, (w, _)| w.strong_count() > 0);
            self.cap = (self.map.len() * 2).max(MEMO_MIN);
        }
        self.map.insert(Arc::as_ptr(arc) as usize, (Arc::downgrade(arc), v));
    }
}

static LAYERS: LazyLock<RwLock<Memo<Layer, LayerInfo>>> = LazyLock::new(|| RwLock::new(Memo::new()));

/// `layer`'s [`LayerInfo`], memoised when `layer` is one of `comp`'s shared layers (a layer
/// from elsewhere is described afresh).
pub(crate) fn layer_info(comp: &Comp, layer: &Layer) -> LayerInfo {
    let addr: *const Layer = layer;
    if let Some(i) = LAYERS.read().ok().and_then(|m| m.get(addr)) {
        return i;
    }
    match comp.layers.arcs().iter().find(|a| std::ptr::eq(Arc::as_ptr(a), addr)) {
        Some(arc) => layer_info_arc(arc),
        None => LayerInfo::of(layer),
    }
}

/// [`layer_info`] of a shared layer.
pub(crate) fn layer_info_arc(arc: &Arc<Layer>) -> LayerInfo {
    if let Some(i) = LAYERS.read().ok().and_then(|m| m.get(Arc::as_ptr(arc))) {
        return i;
    }
    let info = LayerInfo::of(arc);
    if let Ok(mut m) = LAYERS.write() {
        m.put(arc, info);
    }
    info
}

// ------------------------------------------------------------------------------ keys

/// Cache key for the processed (source → masks → effects) buffer of `layer` at the context
/// time, or `None` when the layer must not be cached (precomps and moving footage without a
/// source key, see [`layer_key_src`]; cameras, adjustment layers…).
pub fn layer_key(ctx: &EvalCtx, layer: &Layer, scale: f64, draft: bool, blur: bool) -> Option<u64> {
    layer_key_src(ctx, layer, scale, draft, blur, None)
}

/// [`layer_key`] with the renderer's key for the layer's source (`src`): a precomp's nested
/// content ([`crate::precomp_key`]), or for footage which proxy the render uses. Without it,
/// precomps and footage aren't cached.
pub(crate) fn layer_key_src(ctx: &EvalCtx, layer: &Layer, scale: f64, draft: bool, blur: bool, src: Option<u64>) -> Option<u64> {
    // Time effects see property values at other times. Keyframes are part of the hashed
    // structure (and the layer time is folded in), but expressions may read other layers at
    // those times, which the key cannot see: such layers are not cached.
    let info = layer_info(ctx.comp, layer);
    if info.reads_other_times && info.content_expressions {
        return None;
    }
    key_with(ctx, layer, scale, draft, blur, false, src)
}

/// Cache key for a layer's *input* at the context time: source → masks → its first `effects`
/// effects (see `Renderer::layer_input`). Unlike [`layer_key`], every footage frame is cached
/// here (keyed by item and source time), since Time effects read many neighbouring frames.
pub fn input_key(ctx: &EvalCtx, layer: &Layer, scale: f64, draft: bool, blur: bool, effects: usize) -> Option<u64> {
    input_key_src(ctx, layer, scale, draft, blur, effects, None)
}

/// [`input_key`] with the renderer's source key (see [`layer_key_src`]).
#[allow(clippy::too_many_arguments)]
pub(crate) fn input_key_src(ctx: &EvalCtx, layer: &Layer, scale: f64, draft: bool, blur: bool, effects: usize, src: Option<u64>) -> Option<u64> {
    if layer.switches.adjustment {
        return None;
    }
    let base = key_any(ctx, layer, scale, draft, blur, true, Some(effects), src)?;
    let mut h = KeyHasher(base ^ 0x5bd1_e995_7a3c_11d3);
    effects.hash(&mut h);
    Some(h.finish())
}

/// Does the layer run an effect that reads the layer at other times (Echo, Timewarp…)?
fn reads_other_times(layer: &Layer) -> bool {
    layer.switches.effects
        && layer.effects().is_some_and(|fx| {
            fx.groups().any(|g| g.enabled && matches!(&g.kind, effectcraft_project::GroupKind::Effect { effect } if effect.starts_with("ec.time.")))
        })
}

fn has_expression(g: &PropGroup) -> bool {
    g.children.iter().any(|c| match c {
        Node::Prop(p) => p.has_expression(),
        Node::Group(sub) => has_expression(sub),
    })
}

/// Cache key for an adjustment layer's footprint (its source through its masks, see
/// `Renderer::adjustment_footprint`): the GPU compositor then uploads it once, not every frame.
pub fn footprint_key(ctx: &EvalCtx, layer: &Layer, scale: f64, draft: bool, blur: bool) -> Option<u64> {
    Some(derive(key_any(ctx, layer, scale, draft, blur, false, None, None)?, 0xf007_9417))
}

#[allow(clippy::too_many_arguments)]
fn key_with(ctx: &EvalCtx, layer: &Layer, scale: f64, draft: bool, blur: bool, footage: bool, src: Option<u64>) -> Option<u64> {
    if layer.switches.adjustment {
        return None;
    }
    key_any(ctx, layer, scale, draft, blur, footage, None, src)
}

/// `blur`: the layer is motion blurred in this render (`Renderer::mb_on`). `footage`: key every
/// footage frame (a layer's input); otherwise moving footage is kept only when masks or effects
/// process it, as each frame is used about once. `effects`: key only the first that many
/// effects (a layer's input, see [`input_key`]). `src`: the renderer's source key (see
/// [`layer_key_src`]).
#[allow(clippy::too_many_arguments)]
fn key_any(ctx: &EvalCtx, layer: &Layer, scale: f64, draft: bool, blur: bool, footage: bool, effects: Option<usize>, src: Option<u64>) -> Option<u64> {
    let info = layer_info(ctx.comp, layer);
    let mut h = KeyHasher(0xcbf2_9ce4_8422_2325);
    match &layer.source {
        LayerSource::Solid { item } => {
            let it = ctx.project.item(*item)?;
            let ItemKind::Solid(s) = &it.kind else { return None };
            hash_debug(&mut h, s);
        }
        LayerSource::Text | LayerSource::Shape => {}
        LayerSource::Footage { item } => {
            let it = ctx.project.item(*item)?;
            let ItemKind::Footage(f) = &it.kind else { return None };
            // A still (and a still proxy) shows the same picture at every time.
            let still = f.kind == FootageKind::Still && it.proxy.as_ref().is_none_or(|p| p.footage.kind == FootageKind::Still);
            if !footage && (src.is_none() || !(still || info.masks_or_effects)) {
                return None;
            }
            hash_debug(&mut h, f);
            // A proxy (and its Use Proxy switch) changes the pixels.
            hash_debug(&mut h, &it.proxy);
            item.hash(&mut h);
            if !still {
                ctx.source_time(layer).0.hash(&mut h);
                // Frame blending mixes neighbouring source frames.
                ctx.comp.enable_frame_blending.hash(&mut h);
            }
        }
        // The nested comp's content, from the renderer.
        LayerSource::Comp { .. } if src.is_some() => {}
        _ => return None,
    }
    src.hash(&mut h);
    // Bit depth and colour management change the pixels.
    crate::color::Pipe::of(&ctx.project.settings).key().hash(&mut h);
    ctx.comp_id.hash(&mut h);
    ctx.comp.width.hash(&mut h);
    ctx.comp.height.hash(&mut h);
    scale.to_bits().hash(&mut h);
    draft.hash(&mut h);
    info.head.hash(&mut h);
    // Mask motion blur and effects that read the shutter depend on whether the layer is motion
    // blurred in this render and on the comp's shutter.
    if info.masks_or_effects {
        blur.hash(&mut h);
        ctx.comp.enable_motion_blur.hash(&mut h);
        ctx.comp.shutter_angle.to_bits().hash(&mut h);
        ctx.comp.shutter_phase.to_bits().hash(&mut h);
        ctx.comp.motion_blur_samples.hash(&mut h);
    }
    match effects {
        // Static structure (memoised), then the values of what's animated.
        None => {
            info.content.hash(&mut h);
            if info.content_animated {
                for c in &layer.props.children {
                    if let Node::Group(g) = c
                        && g.match_id != "transform"
                        && g.match_id != effectcraft_project::styles::GROUP
                    {
                        hash_values(&mut h, ctx, layer, g);
                    }
                }
            }
        }
        Some(n) => {
            for c in &layer.props.children {
                // Transform is applied later; Layer Styles are keyed separately (see [`styles_key`]).
                if let Node::Group(g) = c
                    && (g.match_id == "transform" || g.match_id == effectcraft_project::styles::GROUP)
                {
                    continue;
                }
                // A layer's input runs only its first effects: the later ones (a Puppet whose
                // pins are dragged, #212) don't change it, so it stays cached.
                if let Node::Group(g) = c
                    && g.match_id == "effects"
                {
                    let mut first = g.clone();
                    let mut seen = 0;
                    first.children.retain(|e| match e {
                        Node::Prop(_) => true,
                        Node::Group(_) => {
                            seen += 1;
                            seen <= n
                        }
                    });
                    hash_debug(&mut h, &first);
                    hash_values(&mut h, ctx, layer, &first);
                    continue;
                }
                hash_debug(&mut h, c);
                if let Node::Group(g) = c {
                    hash_values(&mut h, ctx, layer, g);
                }
            }
        }
    }
    // Paint strokes appear and disappear with their Duration spans (not property values).
    if info.paint
        && let Some(fx) = layer.effects()
    {
        let lt = layer.layer_time(ctx.time).seconds();
        for g in fx.groups().filter(|g| g.enabled && effectcraft_effects::paint::is_paint(g)) {
            effectcraft_effects::paint::cache_key(g, lt).0.hash(&mut h);
        }
    }
    if info.time_dependent {
        layer.layer_time(ctx.time).0.hash(&mut h);
        // Effects also see the layer's time mapping.
        layer.start_time.0.hash(&mut h);
        layer.stretch.to_bits().hash(&mut h);
    }
    // Effects that see the comp camera or lights (#397) change when they or the layer move.
    if info.reads_comp_scene {
        hash_debug(&mut h, &crate::comp_scene(ctx, layer));
    }
    // Effects that read other layers (layer parameters) see their pixels.
    if info.layer_refs {
        hash_layer_refs(&mut h, ctx, layer, info.reads_other_times, 0)?;
    }
    Some(h.finish())
}

/// Does the layer run an effect that reads the comp camera or lights (Card Dance, CC Particle
/// World…)?
fn reads_comp_scene(layer: &Layer) -> bool {
    layer.switches.effects
        && layer.effects().is_some_and(|fx| {
            fx.groups().any(|g| {
                g.enabled
                    && matches!(&g.kind, effectcraft_project::GroupKind::Effect { effect } if effectcraft_effects::READS_COMP_SCENE.contains(&effect.as_str()))
            })
        })
}

/// Cache key for the styled layer (content key + the Layer Styles group: structure, eye
/// switches, values, keyframes, expressions and their values at the context time — Global Light
/// included, as every layer mirrors it in its Blending Options).
pub fn styles_key(ctx: &EvalCtx, layer: &Layer, content_key: u64) -> u64 {
    let mut h = KeyHasher(content_key ^ 0x9e37_79b9_7f4a_7c15);
    if let Some(g) = layer.layer_styles() {
        let info = layer_info(ctx.comp, layer);
        info.styles.hash(&mut h);
        if info.styles_animated {
            hash_values(&mut h, ctx, layer, g);
        }
    }
    h.finish()
}

/// A sub-key of `key` (styled layer passes, flattened buffer).
pub fn derive(key: u64, i: u64) -> u64 {
    let mut h = KeyHasher(key);
    i.hash(&mut h);
    h.finish()
}
