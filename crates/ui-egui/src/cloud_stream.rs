//! CR6b (`plan/cloud-render.md` CR6): real-time playback from the cloud frame server's playback
//! stream (the server half is CR6a, `effectcraft_automation::frame_stream`; the protocol is
//! [`effectcraft_engine::remote_stream`]).
//!
//! While Preview On is the cloud and a preview plays, the app asks the server to *play* the comp
//! from the playhead instead of asking for frames one by one. The server renders ahead in play
//! order and sends self-contained H.264 chunks of 15 frames; they decode here on threads of
//! their own ([`StreamShared`]) into a [`StreamBuffer`], and every UI frame publishes what is new
//! into the RAM preview cache under the frame keys playback looks for. Playback itself is
//! unchanged: it waits at a frame that isn't cached (never drops), and the sound follows its
//! clock as for local frames.
//!
//! - **Adaptive resolution.** A chunk may come at half or quarter size while the lead is short;
//!   it is cached under the viewer's key all the same (the viewer scales it up) and replaced
//!   when the full-size copy comes. Low-resolution and stale frames are taken out of the cache
//!   when the preview stops, so the paused viewer shows a full frame (CR5's path).
//! - **Edits while playing.** The edit goes to the server as a patch; it answers
//!   [`StreamReply::Stale`] from the first frame the edit can change. Frames from there on keep
//!   playing (marked stale) until the new revision's replace them, rather than holding playback
//!   while the server refills seconds of buffer.
//! - **Fallback.** A failed stream or a lost connection ends the stream; the preview goes on
//!   rendering here with a status note. Never a hang: playback only ever waits for frames, and
//!   the frames it waits for then render on this Mac.
#![cfg(not(target_arch = "wasm32"))]

use std::collections::{BTreeMap, HashSet};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use effectcraft_engine::project::{ItemId, Project};
use effectcraft_engine::render::RenderOpts;
use effectcraft_engine::time::{FrameRate, Tick};
use serde::Serialize;

use crate::cloud::CloudFrames;
use crate::frames::{FrameKey, Frames};

/// Frames per chunk asked for (≈ 0.5 s at 29.97 fps).
pub const CHUNK_FRAMES: u32 = effectcraft_engine::remote_stream::DEFAULT_CHUNK_FRAMES;
/// Seconds of chunks the server keeps ahead of the playhead (CR6a: 8 covers its hitches).
pub const AHEAD_SECS: f32 = 8.0;
/// Threads decoding chunks, and decoder threads each.
const DECODERS: usize = 2;
const DECODER_THREADS: usize = 3;
/// How often the playhead goes to the server while playing (seconds).
const PLAYHEAD_EVERY: f64 = 0.15;

/// One chunk as it arrived.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ChunkMeta {
    pub seq: u64,
    pub revision: u64,
    /// Comp frame of its first frame, and how many frames it has.
    pub first: i64,
    pub frames: u32,
    /// Its size relative to the full size.
    pub scale: f32,
}

/// What the stream did (`ui.inspect` `preview.stream`).
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamStats {
    /// `starting`, `playing`, `ended`, `failed`, `stopped`.
    pub state: String,
    pub error: Option<String>,
    /// Start request → first chunk decoded (ms).
    pub warmup_ms: Option<f64>,
    /// Chunks received, and of those, copies of frames it already had (upgrades, revisions).
    pub chunks: u64,
    pub upgrades: u64,
    /// Chunks decoded after playback reached their first frame (it waited for them), not counting
    /// the first.
    pub late: u64,
    /// Time playback waited for a frame after the warm-up (ms).
    pub stall_ms: f64,
    pub stalls: u64,
    pub bytes: u64,
    /// Received bits per second since the start (kbit/s).
    pub kbps: Option<f64>,
    /// Decode time per frame (ms, smoothed).
    pub decode_ms: Option<f64>,
    /// First copies of frames by resolution: full, half, quarter.
    pub full: u64,
    pub half: u64,
    pub quarter: u64,
    /// `stale` notices (edits while playing), and chunks dropped as broken.
    pub stale: u64,
    pub broken: u64,
    /// Full size of the stream's frames.
    pub size: Option<[u32; 2]>,
}

/// A decoded frame held for playback.
struct Held {
    img: Arc<egui::ColorImage>,
    revision: u64,
    seq: u64,
    scale: f32,
    /// An edit changed it after it was rendered (a newer copy is on its way).
    stale: bool,
    /// The key it was last put in the RAM cache under.
    published: Option<FrameKey>,
}

/// The decoded frames of one stream (the logic, without the network: see the tests).
pub struct StreamBuffer {
    frames: BTreeMap<i64, Held>,
    /// `stale` notices: (revision, first frame): older revisions' frames from there on are old.
    notices: Vec<(u64, i64)>,
    /// Keys this stream put in the cache that are below full size or stale: taken out again when
    /// the preview stops.
    dirty: HashSet<FrameKey>,
    /// Playback's frame and range (see [`StreamBuffer::set_playhead`]).
    cur: i64,
    range: (i64, i64, bool),
    /// Most frames held.
    cap: usize,
    started: Instant,
    pub stats: StreamStats,
    /// The newest chunk received (flow control).
    pub received: Option<u64>,
    /// The stream is over (`failed`): playback goes on here.
    pub failed: Option<String>,
}

impl StreamBuffer {
    pub fn new(cur: i64, range: (i64, i64, bool), cap: usize) -> StreamBuffer {
        StreamBuffer {
            frames: BTreeMap::new(),
            notices: vec![],
            dirty: HashSet::new(),
            cur,
            range,
            cap: cap.max(1),
            started: Instant::now(),
            stats: StreamStats { state: "starting".into(), ..StreamStats::default() },
            received: None,
            failed: None,
        }
    }

    /// Frames playback shows before it reaches `f` (`None`: it doesn't, from where it is).
    fn ahead(&self, f: i64) -> Option<i64> {
        let (start, end, looping) = self.range;
        if f < start || f > end {
            return None;
        }
        if f >= self.cur {
            return Some(f.saturating_sub(self.cur));
        }
        looping.then(|| end.saturating_sub(self.cur).saturating_add(1).saturating_add(f.saturating_sub(start)))
    }

    /// Playback is at `cur`: frames it passed (and won't reach again) go.
    pub fn set_playhead(&mut self, cur: i64) {
        self.cur = cur;
        let cap = self.cap as i64;
        let gone: Vec<i64> = self.frames.keys().copied().filter(|f| self.ahead(*f).is_none_or(|d| d > cap)).collect();
        for f in gone {
            self.frames.remove(&f);
        }
    }

    pub fn has(&self, f: i64) -> bool {
        self.frames.contains_key(&f)
    }

    /// Whether the frame is held and still current.
    pub fn fresh(&self, f: i64) -> bool {
        self.frames.get(&f).is_some_and(|h| !h.stale)
    }

    pub fn len(&self) -> usize {
        self.frames.len()
    }

    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    /// A chunk was received (`bytes` of payload).
    pub fn arrived(&mut self, seq: u64, bytes: usize) {
        self.stats.chunks = self.stats.chunks.saturating_add(1);
        self.stats.bytes = self.stats.bytes.saturating_add(bytes as u64);
        self.received = Some(self.received.map_or(seq, |r| r.max(seq)));
        let secs = self.started.elapsed().as_secs_f64();
        if secs > 0.0 {
            self.stats.kbps = Some(self.stats.bytes as f64 * 8.0 / 1000.0 / secs);
        }
    }

    /// A chunk decoded into `imgs` (its frames in order; fewer than it should have when the tail
    /// didn't decode). The newest copy of a frame wins: a newer revision, else a later chunk; a
    /// copy an edit made old only fills a gap.
    pub fn insert(&mut self, meta: ChunkMeta, imgs: Vec<Arc<egui::ColorImage>>, decode_ms: f64) {
        if self.stats.warmup_ms.is_none() {
            self.stats.warmup_ms = Some(self.started.elapsed().as_secs_f64() * 1000.0);
            self.stats.state = "playing".into();
        } else if meta.first <= self.cur && meta.first.saturating_add(i64::from(meta.frames)) > self.cur && !self.fresh(self.cur) {
            // Playback reached it before it was here.
            self.stats.late = self.stats.late.saturating_add(1);
        }
        if !imgs.is_empty() {
            let per = decode_ms / imgs.len() as f64;
            self.stats.decode_ms = Some(self.stats.decode_ms.map_or(per, |o| o * 0.75 + per * 0.25));
        }
        let mut upgrade = false;
        let mut first_copies = 0u64;
        for (i, img) in imgs.into_iter().enumerate() {
            let f = meta.first.saturating_add(i as i64);
            if self.ahead(f).is_none_or(|d| d > self.cap as i64) {
                continue;
            }
            let stale = self.notices.iter().any(|(r, from)| meta.revision < *r && f >= *from);
            let new = Held { img, revision: meta.revision, seq: meta.seq, scale: meta.scale, stale, published: None };
            match self.frames.get(&f) {
                None => {
                    first_copies += 1;
                    self.frames.insert(f, new);
                }
                Some(old) => {
                    let better = if old.stale != stale { old.stale } else { (meta.revision, meta.seq) > (old.revision, old.seq) };
                    if better {
                        upgrade = true;
                        self.frames.insert(f, new);
                    }
                }
            }
        }
        if upgrade && first_copies == 0 {
            self.stats.upgrades = self.stats.upgrades.saturating_add(1);
        }
        let n = match meta.scale {
            s if s >= 0.99 => &mut self.stats.full,
            s if s >= 0.49 => &mut self.stats.half,
            _ => &mut self.stats.quarter,
        };
        *n = n.saturating_add(first_copies);
    }

    /// An edit at `revision` changed the frames from `from` on.
    pub fn mark_stale(&mut self, revision: u64, from: i64) {
        self.stats.stale = self.stats.stale.saturating_add(1);
        self.notices.push((revision, from));
        if self.notices.len() > 64 {
            self.notices.remove(0);
        }
        for h in self.frames.range_mut(from..).map(|(_, h)| h) {
            if h.revision < revision && !h.stale {
                h.stale = true;
                if let Some(k) = h.published {
                    self.dirty.insert(k);
                }
            }
        }
    }

    /// The frames to put in the RAM cache under `series` (with its frame set): new ones, and
    /// every one again when the series changed (an edit gives the comp a new content key).
    pub fn publish(&mut self, series: &FrameKey) -> Vec<(FrameKey, Arc<egui::ColorImage>)> {
        let mut out = vec![];
        for (f, h) in &mut self.frames {
            let key = FrameKey { frame: *f, ..*series };
            if h.published == Some(key) {
                continue;
            }
            h.published = Some(key);
            if h.stale || h.scale < 0.99 {
                self.dirty.insert(key);
            } else {
                self.dirty.remove(&key);
            }
            out.push((key, h.img.clone()));
        }
        out
    }

    /// The keys to take out of the cache when the preview stops (below full size, stale).
    pub fn take_dirty(&mut self) -> Vec<FrameKey> {
        self.dirty.drain().collect()
    }

    pub fn fail(&mut self, error: String) {
        if self.failed.is_none() {
            self.stats.state = "failed".into();
            self.stats.error = Some(error.clone());
            self.failed = Some(error);
        }
    }
}

/// A stream's state shared by the link thread, its decoders and the app.
pub struct StreamShared {
    pub buffer: Mutex<StreamBuffer>,
    /// Woken when frames are in (the app's UI).
    repaint: Option<egui::Context>,
    /// The comp's frame rate (for `stale` times).
    pub rate: FrameRate,
}

impl StreamShared {
    pub fn new(buffer: StreamBuffer, rate: FrameRate, repaint: Option<egui::Context>) -> Arc<StreamShared> {
        Arc::new(StreamShared { buffer: Mutex::new(buffer), repaint, rate })
    }

    pub fn buffer(&self) -> std::sync::MutexGuard<'_, StreamBuffer> {
        self.buffer.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn wake(&self) {
        if let Some(ctx) = &self.repaint {
            ctx.request_repaint();
        }
    }

    /// The stream is over (the server said so, or the link went).
    pub fn fail(&self, error: String) {
        self.buffer().fail(error);
        self.wake();
    }
}

/// Decodes a stream's chunks on [`DECODERS`] threads of its own, into its buffer. The threads end
/// when this is dropped (the stream is over).
pub struct Decoders {
    tx: Sender<(ChunkMeta, Vec<u8>)>,
}

impl Decoders {
    pub fn start(shared: &Arc<StreamShared>) -> Decoders {
        let (tx, rx) = channel::<(ChunkMeta, Vec<u8>)>();
        let rx = Arc::new(Mutex::new(rx));
        for i in 0..DECODERS {
            let (rx, shared) = (rx.clone(), shared.clone());
            let spawned = std::thread::Builder::new().name(format!("ec-stream-dec-{i}")).spawn(move || decode_loop(&rx, &shared));
            if let Err(e) = spawned {
                log::warn!("cloud stream: no decoder thread: {e}");
            }
        }
        Decoders { tx }
    }

    pub fn push(&self, meta: ChunkMeta, payload: Vec<u8>) {
        let _ = self.tx.send((meta, payload));
    }
}

fn decode_loop(rx: &Arc<Mutex<Receiver<(ChunkMeta, Vec<u8>)>>>, shared: &Arc<StreamShared>) {
    loop {
        let job = rx.lock().unwrap_or_else(PoisonError::into_inner).recv();
        let Ok((meta, payload)) = job else { return };
        let t = Instant::now();
        let decoded = decode(&payload);
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        match decoded {
            Ok(imgs) => {
                if imgs.len() != meta.frames as usize {
                    log::info!("cloud stream: chunk #{} decoded {} of {} frames", meta.seq, imgs.len(), meta.frames);
                }
                shared.buffer().insert(meta, imgs, ms);
            }
            Err(e) => {
                log::warn!("cloud stream: chunk #{}: {e}", meta.seq);
                let mut b = shared.buffer();
                b.stats.broken = b.stats.broken.saturating_add(1);
            }
        }
        shared.wake();
    }
}

/// A chunk's frames as viewer images (opaque, so premultiplied as they are).
pub fn decode(payload: &[u8]) -> Result<Vec<Arc<egui::ColorImage>>, String> {
    let frames = std::panic::catch_unwind(|| effectcraft_media::preview_stream::decode_chunk(payload, DECODER_THREADS))
        .unwrap_or_else(|_| Err("decoding the chunk panicked".into()))?;
    Ok(frames
        .into_iter()
        .filter(|f| f.rgba.len() == (f.width as usize).saturating_mul(f.height as usize).saturating_mul(4) && !f.rgba.is_empty())
        .map(|f| Arc::new(egui::ColorImage::from_rgba_premultiplied([f.width as usize, f.height as usize], &f.rgba)))
        .collect())
}

/// What [`CloudPlayback::tick`] needs from the app each UI frame.
pub struct PlayNow<'a> {
    /// A video preview plays (not caching first).
    pub playing: bool,
    pub now: f64,
    pub comp: ItemId,
    pub rate: FrameRate,
    pub project: &'a Arc<Project>,
    pub revision: u64,
    /// The viewer's frame keys (frame 0) and render options.
    pub series: FrameKey,
    pub opts: RenderOpts,
    /// The frame on screen, the preview's range (inclusive) and looping.
    pub cur: i64,
    pub start: i64,
    pub end: i64,
    pub looping: bool,
    /// Playback waits for a frame.
    pub waiting: bool,
    /// How many viewer frames the RAM preview holds.
    pub fit: i64,
}

/// What the app does with this UI frame's playback.
#[derive(Debug, Default, PartialEq)]
pub struct PlayTick {
    /// The stream delivers this preview's frames: don't render or request them here.
    pub streamed: bool,
    /// A status note (the stream gave up: playing on this Mac).
    pub note: Option<String>,
}

/// The app's side of the playback stream: start it when a cloud preview plays, feed it the
/// playhead and edits, publish its frames, stop it and clean up.
#[derive(Default)]
pub struct CloudPlayback {
    current: Option<Running>,
    /// This preview gave up on the stream (it failed): it plays on here until the next one.
    given_up: bool,
    /// The last stream's numbers (kept after it stops, for `ui.inspect`).
    pub last: Option<StreamStats>,
}

struct Running {
    shared: Arc<StreamShared>,
    client: Arc<CloudFrames>,
    revision: u64,
    cur: i64,
    range: (i64, i64, bool),
    playhead_at: f64,
    seen: f64,
    stalled: bool,
}

impl CloudPlayback {
    /// The running stream's numbers, else the last one's.
    pub fn stats(&self) -> Option<StreamStats> {
        self.current.as_ref().map(|r| r.shared.buffer().stats.clone()).or_else(|| self.last.clone())
    }

    pub fn active(&self) -> bool {
        self.current.is_some()
    }

    /// Follow this UI frame's playback (`client`: the cloud, when the viewer's frames go to it).
    pub fn tick(&mut self, client: Option<&Arc<CloudFrames>>, p: &PlayNow<'_>, frames: &Frames, ctx: Option<&egui::Context>) -> PlayTick {
        let client = client.filter(|c| c.ready());
        if !p.playing {
            self.given_up = false;
            self.halt(frames, Some(FrameKey { frame: p.cur, ..p.series }));
            return PlayTick::default();
        }
        let Some(client) = client else {
            // The cloud went away mid-preview (its own note says so): frames render here.
            self.halt(frames, None);
            return PlayTick::default();
        };
        if self.given_up {
            return PlayTick::default();
        }
        if self.current.as_ref().is_some_and(|r| !Arc::ptr_eq(&r.client, client)) {
            self.halt(frames, None);
        }
        let range = (p.start, p.end, p.looping);
        let Some(r) = &mut self.current else {
            let cap = usize::try_from(p.fit.clamp(1, 100_000)).unwrap_or(1);
            let ahead = AHEAD_SECS.min(((p.fit as f64 * 0.6) / p.rate.as_f64().max(1.0)) as f32).max(1.0);
            let shared = StreamShared::new(StreamBuffer::new(p.cur, range, cap), p.rate, ctx.cloned());
            let start = StreamStart {
                project: p.project.clone(),
                revision: p.revision,
                comp: p.comp,
                from: p.rate.tick_of(p.cur),
                to: Some(p.rate.tick_of(p.end.saturating_add(1))),
                loop_from: Some(p.rate.tick_of(p.start)),
                looping: p.looping,
                opts: p.opts,
                ahead_secs: ahead,
                cache: frames.cache_series(p.project, &p.series, &p.opts),
            };
            client.stream_start(start, shared.clone());
            self.current =
                Some(Running { shared, client: client.clone(), revision: p.revision, cur: p.cur, range, playhead_at: p.now, seen: p.now, stalled: false });
            return PlayTick { streamed: true, note: None };
        };
        let failed = r.shared.buffer().failed.clone();
        if let Some(e) = failed {
            self.given_up = true;
            self.halt(frames, None);
            return PlayTick { streamed: false, note: Some(format!("Cloud playback stream stopped ({e}): playing on this Mac")) };
        }
        // An edit: the server renders again from the first frame it changes.
        if p.revision != r.revision {
            r.revision = p.revision;
            r.client.stream_sync(p.revision, p.project);
        }
        // A jump (the playhead moved by hand, the range changed): play from there.
        let wrapped = p.looping && p.cur <= p.start.saturating_add(2) && r.cur >= p.end.saturating_sub(2);
        let jump = (p.cur < r.cur && !wrapped) || p.cur > r.cur.saturating_add((p.rate.as_f64() * 2.0) as i64).saturating_add(2) || range != r.range;
        if jump {
            let cap = usize::try_from(p.fit.clamp(1, 100_000)).unwrap_or(1);
            let mut b = r.shared.buffer();
            b.range = range;
            b.cap = cap;
            drop(b);
            r.range = range;
            r.client.stream_seek(p.rate.tick_of(p.cur));
        }
        r.cur = p.cur;
        // Stalls after the warm-up.
        let dt = (p.now - r.seen).max(0.0);
        r.seen = p.now;
        let mut b = r.shared.buffer();
        b.set_playhead(p.cur);
        if b.stats.warmup_ms.is_some() {
            if p.waiting {
                if !r.stalled {
                    b.stats.stalls = b.stats.stalls.saturating_add(1);
                }
                b.stats.stall_ms += dt * 1000.0;
            }
            r.stalled = p.waiting;
        }
        let ready = b.publish(&p.series);
        drop(b);
        for (k, img) in ready {
            frames.insert_streamed(k, img);
        }
        if p.now - r.playhead_at >= PLAYHEAD_EVERY || jump {
            r.playhead_at = p.now;
            r.client.stream_playhead(p.rate.tick_of(p.cur), true);
        }
        PlayTick { streamed: true, note: None }
    }

    /// Stop the stream (if any) and take its low-resolution and stale frames out of the cache;
    /// `shown`: the frame on screen, taken out too so the paused viewer gets its exact frame.
    pub fn halt(&mut self, frames: &Frames, shown: Option<FrameKey>) {
        let Some(r) = self.current.take() else { return };
        r.client.stream_stop();
        let mut b = r.shared.buffer();
        if b.failed.is_none() {
            b.stats.state = "stopped".into();
        }
        let dirty = b.take_dirty();
        let streamed_shown = shown.filter(|k| b.frames.get(&k.frame).is_some_and(|h| h.published == Some(*k)));
        self.last = Some(b.stats.clone());
        drop(b);
        for k in dirty.iter().chain(streamed_shown.iter()) {
            frames.remove_frame(k);
        }
    }
}

/// Where a stream plays from (see [`effectcraft_engine::remote_stream::StreamMsg::Start`]).
pub struct StreamStart {
    pub project: Arc<Project>,
    pub revision: u64,
    pub comp: ItemId,
    pub from: Tick,
    pub to: Option<Tick>,
    pub loop_from: Option<Tick>,
    pub looping: bool,
    pub opts: RenderOpts,
    pub ahead_secs: f32,
    /// The viewer's frame series (CR8): the server plays frames it cached and keeps the ones it
    /// renders.
    pub cache: Option<effectcraft_engine::remote::CacheSeries>,
}
