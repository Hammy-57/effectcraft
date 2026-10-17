//! Motion Library panel (Window ▸ Motion Library): the card catalog kept in a remote bucket
//! (`effectcraft_motionlib`), on every machine the app runs on.
//!
//! - Folders on the left, a search field and the cards on the right. Thumbnails download as they
//!   scroll into view, a card's preview plays while the pointer is over it, and applying a card
//!   (double-click or **Apply**) downloads its project and files, then nests its composition at
//!   the playhead (`library.apply`, one undo step).
//! - Cards whose footage stayed on the publishing Mac show **needs footage**: they import with
//!   missing footage items to relink (`footage.relink`).
//! - Opening the panel checks the remote; a newer catalog shows the **Library updated** chip, which
//!   switches to it (`library.update`).
//!
//! Everything here is also a command (`library.*`), so agents use the same library.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use effectcraft_engine::motionlib::{BlobRef, BlobState, Card, Client, FolderRow};
use egui::{Align2, Color32, CornerRadius, Rect, Sense, Stroke, pos2, vec2};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::panel_kit as kit;
use crate::i18n::tr;
use crate::theme::Tokens;
use crate::{EffectcraftApp, widgets};

/// What the panel shows (kept with the workspace; `motionLibrary.view` changes it).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ViewState {
    pub query: String,
    /// Folder path (`""` = all).
    pub folder: String,
    /// The selected card's id.
    pub selected: Option<String>,
    /// Vertical scroll of the cards (points).
    pub scroll: f32,
    /// Interface scale in percent ([`SCALE_RANGE`], steps of 10).
    pub scale: u32,
    /// Card width in points at 100 % ([`CARD_SIZE_RANGE`]).
    pub card_size: f32,
    /// Folders open in the collection tree.
    pub expanded: BTreeSet<String>,
    /// Vertical scroll of the collection tree.
    pub folder_scroll: f32,
    /// The Display settings popover is open.
    #[serde(skip)]
    pub display_open: bool,
    /// The card being applied (downloading) and when it started.
    #[serde(skip)]
    pub applying: Option<String>,
}

impl Default for ViewState {
    fn default() -> Self {
        ViewState {
            query: String::new(),
            folder: String::new(),
            selected: None,
            scroll: 0.0,
            scale: 100,
            card_size: DEFAULT_CARD_SIZE,
            expanded: BTreeSet::new(),
            folder_scroll: 0.0,
            display_open: false,
            applying: None,
        }
    }
}

/// Interface scale limits in percent (as the library's web panel).
pub const SCALE_RANGE: (u32, u32) = (80, 140);
/// Card width limits in points.
pub const CARD_SIZE_RANGE: (f32, f32) = (88.0, 1200.0);
const DEFAULT_CARD_SIZE: f32 = 144.0;
/// At or below this card width, names take two lines and the type line is hidden.
const COMPACT_CARD: f32 = 112.0;

impl ViewState {
    /// Keep settings in range (a hand-edited or old workspace).
    pub fn clamp(&mut self) {
        self.scale = (self.scale.clamp(SCALE_RANGE.0, SCALE_RANGE.1) / 10) * 10;
        self.card_size = if self.card_size.is_finite() { self.card_size.clamp(CARD_SIZE_RANGE.0, CARD_SIZE_RANGE.1) } else { DEFAULT_CARD_SIZE };
        if !self.scroll.is_finite() {
            self.scroll = 0.0;
        }
        if !self.folder_scroll.is_finite() {
            self.folder_scroll = 0.0;
        }
    }
    /// The view as `motionLibrary.view` reports it.
    pub fn to_json(&self) -> Value {
        json!({
            "folder": self.folder,
            "query": self.query,
            "selected": self.selected,
            "scale": self.scale,
            "cardSize": self.card_size,
            "expanded": self.expanded,
            "display": self.display_open,
        })
    }
    /// Open every folder above `folder` so its row shows in the tree.
    pub fn reveal(&mut self, folder: &str) {
        let mut path = String::new();
        let parts: Vec<&str> = folder.split('/').filter(|p| !p.is_empty()).collect();
        for part in parts.iter().take(parts.len().saturating_sub(1)) {
            if !path.is_empty() {
                path.push('/');
            }
            path.push_str(part);
            self.expanded.insert(path.clone());
        }
    }
}

/// Thumbnails kept as textures; the least recently shown go first.
const MAX_THUMBS: usize = 300;
/// Hover preview: frames per second decoded and at most this many (seconds × rate).
const PREVIEW_FPS: f64 = 12.0;
const PREVIEW_FRAMES: usize = 72;
const PREVIEW_SIDE: u32 = 320;

type Thumb = Option<(egui::TextureHandle, [u32; 2])>;
type ThumbPixels = Option<([u32; 2], Vec<u8>)>;
type ThumbJob = Arc<OnceLock<ThumbPixels>>;

static THUMB_JOBS: AtomicUsize = AtomicUsize::new(0);
const MAX_THUMB_JOBS: usize = 3;

/// Counts a running thumbnail decode (also when it ends early).
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
struct ThumbSlot;

impl Drop for ThumbSlot {
    fn drop(&mut self) {
        THUMB_JOBS.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Decode an image's bytes and shrink it to fit `side` × `side` (unpremultiplied RGBA).
fn decode_image(bytes: &[u8], side: u32) -> ThumbPixels {
    let img = image::load_from_memory(bytes).ok()?;
    let img = if img.width().max(img.height()) > side { img.thumbnail(side, side) } else { img };
    let rgba = img.to_rgba8();
    Some(([rgba.width(), rgba.height()], rgba.into_raw()))
}

/// A card's thumbnail texture: downloaded (as it scrolls into view) and decoded off the UI thread.
fn thumbnail(ctx: &egui::Context, client: &Client, blob: &BlobRef, side: u32, budget: &mut u32) -> Thumb {
    let id = egui::Id::new(("ml-thumb", &blob.hash, side));
    if let Some(t) = ctx.data(|d| d.get_temp::<Thumb>(id)) {
        crate::thumb_lru::touch::<Thumb>(ctx, "ml-thumb", id, MAX_THUMBS);
        return t;
    }
    let jid = egui::Id::new(("ml-thumb-job", &blob.hash, side));
    if let Some(job) = ctx.data(|d| d.get_temp::<ThumbJob>(jid)) {
        let done = job.get()?;
        let thumb = done.as_ref().map(|([w, h], px)| {
            let img = egui::ColorImage::from_rgba_unmultiplied([*w as usize, *h as usize], px);
            (ctx.load_texture(format!("ml-{}-{side}", blob.hash), img, egui::TextureOptions::LINEAR), [*w, *h])
        });
        ctx.data_mut(|d| {
            d.insert_temp(id, thumb.clone());
            d.remove::<ThumbJob>(jid);
        });
        crate::thumb_lru::touch::<Thumb>(ctx, "ml-thumb", id, MAX_THUMBS);
        return thumb;
    }
    match client.blob(blob) {
        BlobState::Ready(_) => {}
        BlobState::Loading => return None,
        BlobState::Failed(_) => {
            ctx.data_mut(|d| d.insert_temp::<Thumb>(id, None));
            return None;
        }
    }
    if *budget == 0 || THUMB_JOBS.load(Ordering::Relaxed) >= MAX_THUMB_JOBS {
        return None;
    }
    *budget -= 1;
    let bytes = client.blob_bytes(blob)?;
    let job: ThumbJob = Arc::default();
    ctx.data_mut(|d| d.insert_temp(jid, job.clone()));
    #[cfg(target_arch = "wasm32")]
    {
        let _ = job.set(decode_image(&bytes, side));
        ctx.request_repaint();
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        THUMB_JOBS.fetch_add(1, Ordering::Relaxed);
        let slot = ThumbSlot;
        let ctx2 = ctx.clone();
        let spawned = std::thread::Builder::new().name("ec-library-thumb".into()).spawn(move || {
            let _slot = slot;
            let _ = job.set(decode_image(&bytes, side));
            ctx2.request_repaint();
        });
        if spawned.is_err() {
            ctx.data_mut(|d| d.remove::<ThumbJob>(jid));
        }
    }
    None
}

/// The hover preview being decoded and played.
#[derive(Default)]
struct Preview {
    card: String,
    /// Decoded frames (size, RGBA), appended by the worker.
    frames: Arc<Mutex<Vec<([u32; 2], Vec<u8>)>>>,
    textures: Vec<egui::TextureHandle>,
    done: Arc<AtomicBool>,
    cancel: Arc<AtomicBool>,
    started: Option<f64>,
    error: Option<String>,
}

type PreviewSlot = Arc<Mutex<Preview>>;

fn preview_slot(ctx: &egui::Context) -> PreviewSlot {
    ctx.data_mut(|d| d.get_temp_mut_or_default::<PreviewSlot>(egui::Id::new("ml-preview")).clone())
}

/// Stop the hover preview (the pointer left the card).
fn stop_preview(ctx: &egui::Context) {
    let slot = preview_slot(ctx);
    let Ok(mut p) = slot.lock() else { return };
    if !p.card.is_empty() {
        p.cancel.store(true, Ordering::Relaxed);
        *p = Preview::default();
    }
}

/// The preview frame to draw over `card` now (starting its download and decode on first hover).
fn preview_frame(app: &EffectcraftApp, ctx: &egui::Context, client: &Client, card: &Card) -> Option<egui::TextureHandle> {
    let blob = card.preview.as_ref()?;
    let slot = preview_slot(ctx);
    let mut p = slot.lock().ok()?;
    if p.card != card.id {
        p.cancel.store(true, Ordering::Relaxed);
        *p = Preview { card: card.id.clone(), ..Preview::default() };
    }
    if p.error.is_some() {
        return None;
    }
    if p.started.is_none() {
        match client.blob(blob) {
            BlobState::Loading => {
                ctx.request_repaint_after(std::time::Duration::from_millis(100));
                return None;
            }
            BlobState::Failed(e) => {
                p.error = Some(e);
                return None;
            }
            BlobState::Ready(path) => {
                let (Some(path), Some(imp)) = (path, app.session.importer.clone()) else {
                    p.error = Some("no video decoder here".into());
                    return None;
                };
                let footage = app.session.footage.clone();
                let (frames, done, cancel, ctx2) = (p.frames.clone(), p.done.clone(), p.cancel.clone(), ctx.clone());
                let path = path.to_string_lossy().to_string();
                let work = move || {
                    decode_preview(&*imp, &*footage, &path, &frames, &cancel, &ctx2);
                    done.store(true, Ordering::Relaxed);
                    ctx2.request_repaint();
                };
                #[cfg(target_arch = "wasm32")]
                work();
                #[cfg(not(target_arch = "wasm32"))]
                if std::thread::Builder::new().name("ec-library-preview".into()).spawn(work).is_err() {
                    p.error = Some("no thread for the preview".into());
                    return None;
                }
                p.started = Some(ctx.input(|i| i.time));
            }
        }
    }
    // New frames to textures.
    let fresh: Vec<([u32; 2], Vec<u8>)> = p.frames.lock().ok().map(|mut f| std::mem::take(&mut *f)).unwrap_or_default();
    for ([w, h], px) in fresh {
        let img = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &px);
        let n = p.textures.len();
        p.textures.push(ctx.load_texture(format!("ml-preview-{}-{n}", card.id), img, egui::TextureOptions::LINEAR));
    }
    let n = p.textures.len();
    if n == 0 {
        if p.done.load(Ordering::Relaxed) {
            p.error = Some("the preview has no frames".into());
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(50));
        return None;
    }
    let now = ctx.input(|i| i.time);
    let k = ((now - p.started.unwrap_or(now)).max(0.0) * PREVIEW_FPS) as usize;
    let done = p.done.load(Ordering::Relaxed);
    let i = if done { k % n } else { k.min(n - 1) };
    ctx.request_repaint_after(std::time::Duration::from_secs_f64(1.0 / PREVIEW_FPS));
    p.textures.get(i).cloned()
}

/// Decode a preview movie's first seconds at [`PREVIEW_FPS`], appending frames as they come.
fn decode_preview(
    imp: &dyn effectcraft_engine::Importer,
    footage: &dyn effectcraft_engine::render::FootageSource,
    path: &str,
    frames: &Mutex<Vec<([u32; 2], Vec<u8>)>>,
    cancel: &AtomicBool,
    ctx: &egui::Context,
) {
    let Ok(f) = imp.probe(path) else { return };
    if !f.has_video {
        return;
    }
    let secs = f.duration.seconds();
    let count = ((secs * PREVIEW_FPS).floor() as usize).clamp(1, PREVIEW_FRAMES);
    let item = effectcraft_engine::project::ItemId(u64::MAX - 7);
    for k in 0..count {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let t = effectcraft_engine::time::Tick::from_seconds_f64(k as f64 / PREVIEW_FPS);
        let Some(img) = footage.frame(item, &f, t) else { break };
        let (w, h) = (img.width.max(1), img.height.max(1));
        let s = (PREVIEW_SIDE as f64 / w.max(h) as f64).min(1.0);
        let small = if s < 1.0 {
            effectcraft_engine::raster::resample(&img, ((w as f64 * s).round() as u32).max(1), ((h as f64 * s).round() as u32).max(1))
        } else {
            (*img).clone()
        };
        if let Ok(mut v) = frames.lock() {
            v.push(([small.width, small.height], small.to_rgba8()));
        }
        ctx.request_repaint();
    }
    footage.forget(path);
}

/// The adopted catalog's identity (a new one after an update).
fn generation(client: &Client) -> u64 {
    client.catalog().map_or(0, |c| Arc::as_ptr(&c) as usize as u64)
}

/// Cards matching the view, cached per catalog generation, query and folder. Browsing everything
/// (no folder, no query) groups them by folder in the library's order.
fn visible_cards(ctx: &egui::Context, client: &Client, view: &ViewState) -> Arc<Vec<Card>> {
    type Cached = (u64, String, String, Arc<Vec<Card>>);
    let id = egui::Id::new("ml-cards");
    let generation = generation(client);
    if let Some((g, q, f, cards)) = ctx.data(|d| d.get_temp::<Cached>(id))
        && g == generation
        && q == view.query
        && f == view.folder
    {
        return cards;
    }
    let cards: Vec<Card> = client
        .catalog()
        .map(|cat| {
            let mut found: Vec<Card> = effectcraft_engine::motionlib::search(&cat.cards, &view.query, &view.folder).into_iter().cloned().collect();
            if grouped(view) {
                let ranks = cat.folder_ranks();
                found.sort_by_key(|c| ranks.get(&c.category).copied().unwrap_or(usize::MAX));
            }
            found
        })
        .unwrap_or_default();
    let cards = Arc::new(cards);
    ctx.data_mut(|d| d.insert_temp::<Cached>(id, (generation, view.query.clone(), view.folder.clone(), cards.clone())));
    cards
}

/// Cards show in sections per folder (browsing everything, no search).
fn grouped(view: &ViewState) -> bool {
    view.folder.is_empty() && view.query.trim().is_empty()
}

/// The collection tree, cached per catalog generation.
fn folder_tree(ctx: &egui::Context, client: &Client) -> Arc<Vec<FolderRow>> {
    type Cached = (u64, Arc<Vec<FolderRow>>);
    let id = egui::Id::new("ml-tree");
    let generation = generation(client);
    if let Some((g, rows)) = ctx.data(|d| d.get_temp::<Cached>(id))
        && g == generation
    {
        return rows;
    }
    let rows = Arc::new(client.catalog().map(|c| c.tree()).unwrap_or_default());
    ctx.data_mut(|d| d.insert_temp::<Cached>(id, (generation, rows.clone())));
    rows
}

/// The rows of the tree whose parents are all open.
fn shown_rows<'a>(rows: &'a [FolderRow], expanded: &BTreeSet<String>) -> Vec<&'a FolderRow> {
    rows.iter()
        .filter(|r| {
            let mut path = r.path.as_str();
            while let Some((parent, _)) = path.rsplit_once('/') {
                if !expanded.contains(parent) {
                    return false;
                }
                path = parent;
            }
            true
        })
        .collect()
}

fn draw_text_clipped(p: &egui::Painter, r: Rect, text: &str, font: egui::FontId, color: Color32) {
    let galley = p.layout_no_wrap(text.to_string(), font, color);
    let pos = pos2(r.min.x, r.center().y - galley.size().y / 2.0);
    p.with_clip_rect(r.intersect(p.clip_rect())).galley(pos, galley, color);
}

/// `text` cut to `width` with an ellipsis.
fn elide(p: &egui::Painter, text: &str, font: &egui::FontId, width: f32) -> String {
    let fits = |s: &str| p.layout_no_wrap(s.to_string(), font.clone(), Color32::WHITE).size().x <= width;
    if fits(text) {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    let (mut lo, mut hi) = (0usize, chars.len());
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        let s: String = chars.iter().take(mid).collect::<String>() + "…";
        if fits(&s) { lo = mid } else { hi = mid - 1 }
    }
    chars.iter().take(lo).collect::<String>() + "…"
}

/// Where a card or section header sits in the scrolled content (x from the grid's left edge, y
/// from the top of the content).
enum Placed {
    Header { folder: String, count: usize, rect: Rect },
    Card { index: usize, rect: Rect, thumb_h: f32 },
}

struct Layout {
    items: Vec<Placed>,
    height: f32,
    col_w: f32,
}

/// The height of a card's thumbnail at `col_w` (its own shape; 16:10 without a thumbnail).
fn thumb_height(card: &Card, col_w: f32) -> f32 {
    let aspect = card.thumb_size.filter(|[w, h]| *w > 0 && *h > 0).map_or(10.0 / 16.0, |[w, h]| h as f32 / w as f32);
    (col_w * aspect.clamp(0.2, 2.5)).round()
}

/// Cards in columns of equal width: in folder sections, rows as tall as their tallest card
/// (`grouped`), else packed into the shortest column (search results, one folder).
fn layout(cards: &[Card], grouped: bool, width: f32, card_w: f32, gap: f32, caption_h: f32, header_h: f32) -> Layout {
    let cols = (((width + gap) / (card_w.min(width) + gap)).floor() as usize).max(1);
    let col_w = ((width - gap * (cols as f32 - 1.0)) / cols as f32).max(8.0);
    let mut items = Vec::with_capacity(cards.len() + 16);
    let mut y = 0.0f32;
    if grouped {
        let mut i = 0;
        while i < cards.len() {
            let folder = cards.get(i).map(|c| c.category.clone()).unwrap_or_default();
            let n = cards.iter().skip(i).take_while(|c| c.category == folder).count().max(1);
            items.push(Placed::Header { folder, count: n, rect: Rect::from_min_size(pos2(0.0, y), vec2(width, header_h)) });
            y += header_h + gap;
            for row_start in (i..i + n).step_by(cols) {
                let mut row_h = 0.0f32;
                for (col, index) in (row_start..(row_start + cols).min(i + n)).enumerate() {
                    let Some(card) = cards.get(index) else { break };
                    let th = thumb_height(card, col_w);
                    let h = th + caption_h;
                    row_h = row_h.max(h);
                    items.push(Placed::Card { index, rect: Rect::from_min_size(pos2(col as f32 * (col_w + gap), y), vec2(col_w, h)), thumb_h: th });
                }
                y += row_h + gap;
            }
            i += n;
        }
    } else {
        let mut heights = vec![0.0f32; cols];
        for (index, card) in cards.iter().enumerate() {
            let (col, top) = heights.iter().copied().enumerate().fold((0, f32::INFINITY), |best, (c, h)| if h < best.1 { (c, h) } else { best });
            let th = thumb_height(card, col_w);
            let h = th + caption_h;
            items.push(Placed::Card { index, rect: Rect::from_min_size(pos2(col as f32 * (col_w + gap), top), vec2(col_w, h)), thumb_h: th });
            if let Some(slot) = heights.get_mut(col) {
                *slot = top + h + gap;
            }
        }
        y = heights.iter().copied().fold(0.0, f32::max);
    }
    Layout { items, height: y, col_w }
}

/// A texture drawn to fill `r` (cropped to its shape, as CSS `object-fit: cover`).
fn paint_cover(p: &egui::Painter, tex: egui::TextureId, size: egui::Vec2, r: Rect) {
    let (tw, th) = (size.x.max(1.0), size.y.max(1.0));
    let (ra, ta) = (r.width() / r.height().max(1.0), tw / th);
    let uv = if ta > ra {
        let w = ra / ta;
        Rect::from_min_max(pos2((1.0 - w) / 2.0, 0.0), pos2((1.0 + w) / 2.0, 1.0))
    } else {
        let h = ta / ra;
        Rect::from_min_max(pos2(0.0, (1.0 - h) / 2.0), pos2(1.0, (1.0 + h) / 2.0))
    };
    p.image(tex, r, uv, Color32::WHITE);
}

/// Thumbnail decode size for a card `w` points wide (a few sizes, so zooming reuses textures).
fn thumb_side(w: f32, ppp: f32) -> u32 {
    match (w * ppp).ceil() as u32 {
        0..=160 => 160,
        161..=320 => 320,
        321..=640 => 640,
        _ => 1280,
    }
}

/// A folder's icon (its image, else the folder glyph).
fn paint_folder_icon(ctx: &egui::Context, client: &Client, p: &egui::Painter, row: Option<&FolderRow>, r: Rect, color: Color32, budget: &mut u32) {
    let Some(row) = row else {
        crate::icons::paint(p, r, crate::icons::Icon::Grid, color);
        return;
    };
    if let Some(blob) = &row.icon
        && let Some((tex, [w, h])) = thumbnail(ctx, client, blob, 64, budget)
    {
        let s = (r.width() / w.max(1) as f32).min(r.height() / h.max(1) as f32);
        let ir = Rect::from_center_size(r.center(), vec2(w as f32, h as f32) * s);
        p.image(tex.id(), ir, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
        return;
    }
    crate::icons::paint(p, r, crate::icons::Icon::Folder, color);
}

/// Display settings (interface scale, card size), under the header's display button.
fn display_popover(app: &mut EffectcraftApp, ctx: &egui::Context, anchor: Rect, view: &mut ViewState, s: f32) {
    let t = app.tokens;
    let size = vec2(236.0 * s, 104.0 * s);
    let pos = pos2((anchor.max.x - size.x).max(4.0), anchor.max.y + 6.0);
    let area = egui::Area::new(egui::Id::new("ml-display")).order(egui::Order::Foreground).fixed_pos(pos).show(ctx, |ui| {
        let (r, _) = ui.allocate_exact_size(size, Sense::click());
        let p = ui.painter();
        p.rect_filled(r, CornerRadius::same(8), t.header_bg);
        p.rect_stroke(r, CornerRadius::same(8), Stroke::new(1.0, t.separator), egui::StrokeKind::Inside);
        let pad = 12.0 * s;
        p.text(pos2(r.min.x + pad, r.min.y + 16.0 * s), Align2::LEFT_CENTER, tr("Display settings"), Tokens::semibold(12.0 * s), t.text);
        // Interface scale: − 100% +.
        let y1 = r.min.y + 44.0 * s;
        p.text(pos2(r.min.x + pad, y1), Align2::LEFT_CENTER, tr("Interface scale"), Tokens::ui(11.5 * s), t.text_dim);
        let bw = 24.0 * s;
        let plus = Rect::from_center_size(pos2(r.max.x - pad - bw / 2.0, y1), vec2(bw, 22.0 * s));
        let value = Rect::from_center_size(pos2(plus.min.x - 26.0 * s, y1), vec2(44.0 * s, 22.0 * s));
        let minus = Rect::from_center_size(pos2(value.min.x - bw / 2.0 - 2.0 * s, y1), vec2(bw, 22.0 * s));
        if kit::button(app, ui, minus, "motionLibrary.scaleDown", "−", false) && view.scale > SCALE_RANGE.0 {
            view.scale -= 10;
        }
        if kit::button(app, ui, plus, "motionLibrary.scaleUp", "+", false) && view.scale < SCALE_RANGE.1 {
            view.scale += 10;
        }
        let pct = format!("{}%", view.scale);
        ui.painter().text(value.center(), Align2::CENTER_CENTER, &pct, Tokens::medium(11.5 * s), t.text);
        app.auto.add("motionLibrary.scale", value, &pct);
        // Card size: a slider.
        let y2 = r.min.y + 78.0 * s;
        ui.painter().text(pos2(r.min.x + pad, y2), Align2::LEFT_CENTER, tr("Card size"), Tokens::ui(11.5 * s), t.text_dim);
        let track = Rect::from_min_max(pos2(r.min.x + pad + 76.0 * s, y2 - 8.0 * s), pos2(r.max.x - pad, y2 + 8.0 * s));
        let resp = ui.interact(track, egui::Id::new("ml-card-size"), Sense::click_and_drag());
        let (lo, hi) = CARD_SIZE_RANGE;
        if (resp.dragged() || resp.clicked())
            && let Some(x) = resp.interact_pointer_pos().map(|p| p.x)
        {
            let f = ((x - track.min.x) / track.width().max(1.0)).clamp(0.0, 1.0);
            view.card_size = (lo + f * (hi - lo)).round();
        }
        let f = ((view.card_size - lo) / (hi - lo)).clamp(0.0, 1.0);
        let line = Rect::from_min_max(pos2(track.min.x, y2 - 1.5 * s), pos2(track.max.x, y2 + 1.5 * s));
        let p = ui.painter();
        p.rect_filled(line, CornerRadius::same(2), t.field_border);
        p.rect_filled(Rect::from_min_max(line.min, pos2(track.min.x + f * track.width(), line.max.y)), CornerRadius::same(2), t.accent);
        p.circle_filled(pos2(track.min.x + f * track.width(), y2), 6.0 * s, if resp.hovered() || resp.dragged() { t.accent_hover } else { t.accent });
        app.auto.add_response("motionLibrary.cardSize", &resp, &format!("{}", view.card_size));
        r
    });
    // Close on Escape or a click outside (the display button toggles it itself).
    let outside = ctx.input(|i| i.pointer.any_pressed() && i.pointer.interact_pos().is_some_and(|p| !area.inner.contains(p) && !anchor.contains(p)));
    if outside || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        view.display_open = false;
    }
}

pub fn show(app: &mut EffectcraftApp, ui: &mut egui::Ui, rect: Rect) {
    let t = app.tokens;
    let ctx = ui.ctx().clone();
    let p = ui.painter().with_clip_rect(rect);
    p.rect_filled(rect, 0.0, t.panel_bg);
    let client = match app.session.motion_library() {
        Ok(c) => c,
        Err(e) => {
            let r = rect.shrink(16.0);
            p.text(pos2(r.min.x, r.min.y + 10.0), Align2::LEFT_CENTER, tr("Motion Library isn't set up on this machine"), Tokens::semibold(12.5), t.text);
            let galley = p.layout(e, Tokens::ui(11.5), t.text_dim, r.width().max(80.0));
            p.galley(pos2(r.min.x, r.min.y + 28.0), galley, t.text_dim);
            app.auto.add("motionLibrary.setup", r, "Motion Library isn't set up");
            return;
        }
    };
    // First show: check the remote (the cached catalog shows meanwhile), and repaint on downloads.
    let opened = egui::Id::new("ml-opened");
    if !ctx.data(|d| d.get_temp::<bool>(opened).unwrap_or(false)) {
        ctx.data_mut(|d| d.insert_temp(opened, true));
        let c2 = ctx.clone();
        client.set_notify(Arc::new(move || c2.request_repaint()));
        kit::exec(app, "library.sync", json!({"wait": false}));
    }
    let status = client.status();
    let mut view = app.ui.motion_library.clone();
    view.clamp();
    let s = view.scale as f32 / 100.0;
    let ppp = ctx.pixels_per_point();
    let mut budget = 6u32;
    let total = client.catalog().map_or(0, |c| c.cards.len());
    let tree = folder_tree(&ctx, &client);
    let footer_h = 32.0 * s;
    let narrow = rect.width() < 460.0;

    // Collections: a tree on the left (a dropdown when the panel is narrow).
    let mut main = Rect::from_min_max(rect.min, pos2(rect.max.x, rect.max.y - footer_h));
    if !narrow {
        let side_w = (184.0 * s).min(rect.width() * 0.38).round();
        let side = Rect::from_min_max(rect.min, pos2(rect.min.x + side_w, rect.max.y - footer_h));
        main.min.x = side.max.x + 1.0;
        p.line_segment([side.right_top(), side.right_bottom()], Stroke::new(1.0, t.separator));
        let head_h = 30.0 * s;
        p.text(pos2(side.min.x + 12.0 * s, side.min.y + head_h / 2.0), Align2::LEFT_CENTER, tr("COLLECTIONS"), Tokens::semibold(10.0 * s), t.text_faint);
        let list = Rect::from_min_max(pos2(side.min.x, side.min.y + head_h), side.max);
        let sp = p.with_clip_rect(list);
        let row_h = (26.0 * s).round();
        let rows = shown_rows(&tree, &view.expanded);
        let content = (rows.len() + 1) as f32 * row_h + 8.0 * s;
        if ui.rect_contains_pointer(list) {
            let dy = ctx.input(|i| i.smooth_scroll_delta.y);
            if dy.is_finite() && dy != 0.0 {
                view.folder_scroll -= dy;
            }
        }
        view.folder_scroll = view.folder_scroll.clamp(0.0, (content - list.height()).max(0.0));
        let mut toggle: Option<String> = None;
        for (i, row) in std::iter::once(None).chain(rows.iter().map(|r| Some(*r))).enumerate() {
            let y = list.min.y + i as f32 * row_h - view.folder_scroll;
            if y + row_h < list.min.y || y > list.max.y {
                continue;
            }
            let r = Rect::from_min_size(pos2(side.min.x + 6.0 * s, y), vec2(side.width() - 12.0 * s, row_h - 2.0 * s));
            let (path, name, count, depth) = match row {
                Some(row) => (row.path.as_str(), row.name.as_str(), row.cards, row.depth),
                None => ("", tr("All motion"), total, 0),
            };
            let auto_id = format!("motionLibrary.folder.{}", if path.is_empty() { "all" } else { path });
            let resp = ui.interact(r.intersect(list), egui::Id::new(("ml-folder", path)), Sense::click());
            let active = view.folder == path;
            if active {
                sp.rect_filled(r, CornerRadius::same((5.0 * s) as u8), t.accent.gamma_multiply(0.28));
            } else if resp.hovered() {
                sp.rect_filled(r, CornerRadius::same((5.0 * s) as u8), t.hover);
            }
            let x0 = r.min.x + depth as f32 * 12.0 * s;
            // The open/close arrow of folders with subfolders.
            let arrow = Rect::from_min_size(pos2(x0, r.min.y), vec2(14.0 * s, r.height()));
            if let Some(row) = row
                && row.has_children
            {
                let open = view.expanded.contains(path);
                let icon = if open { crate::icons::Icon::ChevronDown } else { crate::icons::Icon::ChevronRight };
                crate::icons::paint(&sp, Rect::from_center_size(arrow.center(), vec2(10.0 * s, 10.0 * s)), icon, t.text_faint);
                let aresp = ui.interact(arrow.intersect(list), egui::Id::new(("ml-folder-toggle", path)), Sense::click());
                app.auto.add(&format!("motionLibrary.folderToggle.{path}"), arrow.intersect(list), if open { "Close folder" } else { "Open folder" });
                if aresp.clicked() {
                    toggle = Some(path.to_string());
                }
            }
            let icon_r = Rect::from_center_size(pos2(arrow.max.x + 9.0 * s, r.center().y), vec2(16.0 * s, 16.0 * s));
            let color = if active || resp.hovered() { t.text } else { t.text_dim };
            paint_folder_icon(&ctx, &client, &sp, row, icon_r, color, &mut budget);
            let count_s = count.to_string();
            let count_w = sp.layout_no_wrap(count_s.clone(), Tokens::ui(10.0 * s), t.text_faint).size().x;
            sp.text(pos2(r.max.x - 6.0 * s, r.center().y), Align2::RIGHT_CENTER, &count_s, Tokens::ui(10.0 * s), if active { t.text } else { t.text_faint });
            let name_r = Rect::from_min_max(pos2(icon_r.max.x + 7.0 * s, r.min.y), pos2(r.max.x - count_w - 12.0 * s, r.max.y));
            let font = Tokens::ui(12.0 * s);
            let shown = elide(&sp, name, &font, name_r.width().max(4.0));
            draw_text_clipped(&sp, name_r, &shown, font, if active { t.text } else { t.text_dim });
            app.auto.add(&auto_id, r.intersect(list), name);
            if resp.clicked() && toggle.is_none() {
                let has_children = row.is_some_and(|r| r.has_children);
                if active && has_children {
                    // Clicking the open folder again folds it.
                    toggle = Some(path.to_string());
                } else {
                    view.folder = path.to_string();
                    view.scroll = 0.0;
                    if has_children {
                        view.expanded.insert(path.to_string());
                    }
                }
            }
        }
        if let Some(path) = toggle
            && !view.expanded.remove(&path)
        {
            view.expanded.insert(path);
        }
    }

    // Header: the collection and its count, the update chip or status, search, refresh, display.
    let pad = 10.0 * s;
    let head_h = 36.0 * s;
    let head = Rect::from_min_size(main.min, vec2(main.width(), head_h));
    let cards = visible_cards(&ctx, &client, &view);
    let bh = 26.0 * s;
    let display_r = Rect::from_min_size(pos2(head.max.x - pad - bh, head.center().y - bh / 2.0), vec2(bh, bh));
    let refresh_r = display_r.translate(vec2(-(bh + 4.0 * s), 0.0));
    let search_w = (220.0 * s).min((head.width() * 0.45).max(80.0));
    let search_r = Rect::from_min_size(pos2(refresh_r.min.x - 6.0 * s - search_w, head.center().y - 12.0 * s), vec2(search_w, 24.0 * s));
    let mut q = view.query.clone();
    let resp = widgets::search_field(ui, search_r, &mut q, "Search assets", &t);
    app.auto.add_response("motionLibrary.search", &resp, "Search assets");
    if q != view.query {
        view.query = q;
        view.scroll = 0.0;
    }
    if kit::button(app, ui, refresh_r, "motionLibrary.refresh", if status.refreshing { "…" } else { "↻" }, false) {
        kit::exec(app, "library.sync", json!({"wait": false}));
    }
    if kit::button(app, ui, display_r, "motionLibrary.display", "Aa", view.display_open) {
        view.display_open = !view.display_open;
    }
    let title = if view.folder.is_empty() { tr("All motion").to_string() } else { view.folder.rsplit('/').next().unwrap_or(&view.folder).to_string() };
    let title_font = Tokens::semibold(13.0 * s);
    let left = Rect::from_min_max(pos2(head.min.x + pad, head.min.y), pos2(search_r.min.x - 8.0 * s, head.max.y));
    let title_s = elide(&p, &title, &title_font, (left.width() * 0.6).max(10.0));
    let title_w = p.layout_no_wrap(title_s.clone(), title_font.clone(), t.text).size().x;
    draw_text_clipped(&p, left, &title_s, title_font, t.text);
    let count_s = format!("{} {}", cards.len(), if cards.len() == 1 { "item" } else { "items" });
    let count_r = Rect::from_min_max(pos2(left.min.x + title_w + 8.0 * s, left.min.y), left.max);
    draw_text_clipped(&p, count_r, &count_s, Tokens::ui(11.0 * s), t.text_faint);
    app.auto.add("motionLibrary.count", count_r, &count_s);
    let count_w = p.layout_no_wrap(count_s, Tokens::ui(11.0 * s), t.text_faint).size().x;
    let info = Rect::from_min_max(pos2(count_r.min.x + count_w + 10.0 * s, head.center().y - 10.0 * s), pos2(left.max.x, head.center().y + 10.0 * s));
    if let Some(u) = &status.update {
        let w = (p.layout_no_wrap(u.label.clone(), Tokens::medium(11.0 * s), Color32::WHITE).size().x + 20.0 * s).min(info.width().max(0.0));
        let chip = Rect::from_min_size(info.min, vec2(w, info.height()));
        if w > 30.0 * s {
            let resp = ui.interact(chip, egui::Id::new("ml-update-chip"), Sense::click());
            p.rect_filled(chip, CornerRadius::same((10.0 * s) as u8), if resp.hovered() { t.accent_hover } else { t.accent });
            draw_text_clipped(&p, chip.shrink2(vec2(10.0 * s, 0.0)), &u.label, Tokens::medium(11.0 * s), Color32::WHITE);
            app.auto.add("motionLibrary.updateChip", chip, &u.label);
            if resp.on_hover_text(tr("Switch to the newest library")).clicked() {
                kit::exec(app, "library.update", json!({}));
                view.scroll = 0.0;
            }
        }
    } else {
        let (text, color) = match (&status.error, status.cards) {
            (Some(e), _) => (e.clone(), t.warning),
            (None, 0) if status.refreshing || !status.checked => (tr("Loading the library…").to_string(), t.text_dim),
            (None, _) if status.downloading > 0 => (format!("downloading {}", status.downloading), t.text_faint),
            (None, _) => (format!("version {}", status.version), t.text_faint),
        };
        draw_text_clipped(&p, info, &text, Tokens::ui(10.5 * s), color);
        app.auto.add("motionLibrary.status", info, &text);
    }
    let mut grid = Rect::from_min_max(pos2(main.min.x, head.max.y), main.max);
    if narrow {
        let dr = Rect::from_min_size(pos2(grid.min.x + pad, grid.min.y), vec2((grid.width() - 2.0 * pad).max(60.0), 22.0 * s));
        let rows = shown_rows(&tree, &view.expanded);
        let mut paths: Vec<String> = vec![String::new()];
        let mut labels: Vec<String> = vec![format!("{}  ({total})", tr("All motion"))];
        for r in tree.iter() {
            paths.push(r.path.clone());
            labels.push(format!("{}{}  ({})", "  ".repeat(r.depth), r.name, r.cards));
        }
        let _ = rows;
        let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
        let cur = paths.iter().position(|f| *f == view.folder).unwrap_or(0);
        if let Some(i) = kit::dropdown(app, ui, dr, "motionLibrary.folder", &refs, cur)
            && let Some(f) = paths.get(i)
        {
            view.folder = f.clone();
            view.scroll = 0.0;
        }
        grid.min.y += 28.0 * s;
    }

    // Cards: folder sections (browsing everything) or packed columns.
    let gp = p.with_clip_rect(grid);
    let gap = (8.0 * s).round();
    let inner = grid.shrink2(vec2(pad, 0.0));
    let card_w = view.card_size * s;
    let compact = view.card_size <= COMPACT_CARD;
    let caption_h = if compact { 38.0 * s } else { 42.0 * s };
    let header_h = 30.0 * s;
    let is_grouped = grouped(&view);
    let lay = layout(&cards, is_grouped, (inner.width() - 6.0).max(40.0), card_w, gap, caption_h, header_h);
    let content_h = lay.height + gap * 2.0;
    let max_scroll = (content_h - grid.height()).max(0.0);
    if ui.rect_contains_pointer(grid) && !view.display_open {
        let dy = ctx.input(|i| i.smooth_scroll_delta.y);
        if dy.is_finite() && dy != 0.0 {
            view.scroll -= dy;
        }
    }
    view.scroll = view.scroll.clamp(0.0, max_scroll);
    if cards.is_empty() {
        let msg = if total == 0 { "" } else { "No matching motion" };
        gp.text(grid.center(), Align2::CENTER_CENTER, tr(msg), Tokens::ui(12.0 * s), t.text_faint);
    }
    let origin = pos2(inner.min.x, inner.min.y + gap - view.scroll);
    let side = thumb_side(lay.col_w, ppp);
    let radius = CornerRadius::same((6.0 * s) as u8);
    let mut hovered_card = None;
    let mut apply_now: Option<String> = None;
    for item in &lay.items {
        match item {
            Placed::Header { folder, count, rect: hr } => {
                let r = hr.translate(origin.to_vec2());
                if r.max.y < grid.min.y || r.min.y > grid.max.y {
                    continue;
                }
                let n = format!("{count} {}", if *count == 1 { "item" } else { "items" });
                let nw = gp.layout_no_wrap(n.clone(), Tokens::ui(10.0 * s), t.text_faint).size().x;
                gp.text(pos2(r.max.x - 2.0, r.max.y - 9.0 * s), Align2::RIGHT_CENTER, &n, Tokens::ui(10.0 * s), t.text_faint);
                let font = Tokens::semibold(11.5 * s);
                let name = elide(&gp, folder, &font, (r.width() - nw - 16.0 * s).max(10.0));
                gp.text(pos2(r.min.x + 2.0, r.max.y - 9.0 * s), Align2::LEFT_CENTER, &name, font, t.text);
                gp.line_segment([pos2(r.min.x, r.max.y), pos2(r.max.x, r.max.y)], Stroke::new(1.0, t.separator));
                app.auto.add(&format!("motionLibrary.section.{folder}"), r.intersect(grid), &format!("{folder} · {n}"));
            }
            Placed::Card { index, rect: cr, thumb_h } => {
                let tile = cr.translate(origin.to_vec2());
                if tile.max.y < grid.min.y || tile.min.y > grid.max.y {
                    continue;
                }
                let Some(card) = cards.get(*index) else { continue };
                let thumb_r = Rect::from_min_size(tile.min, vec2(tile.width(), *thumb_h));
                let resp = ui.interact(tile.intersect(grid), egui::Id::new(("ml-card", &card.id)), Sense::click());
                let selected = view.selected.as_deref() == Some(card.id.as_str());
                let hovered = resp.hovered();
                gp.rect_filled(tile, radius, if hovered { t.hover } else { t.row_alt });
                let tr_radius = CornerRadius { nw: radius.nw, ne: radius.ne, sw: 0, se: 0 };
                gp.rect_filled(thumb_r, tr_radius, t.field_bg);
                let live = if hovered { preview_frame(app, &ctx, &client, card) } else { None };
                if hovered {
                    hovered_card = Some(card.id.clone());
                }
                let tex = live.map(|t| (t.id(), t.size_vec2())).or_else(|| {
                    let thumb = card.thumbnail.as_ref().and_then(|b| thumbnail(&ctx, &client, b, side, &mut budget));
                    thumb.map(|(tex, [w, h])| (tex.id(), vec2(w as f32, h as f32)))
                });
                match tex {
                    Some((tid, size)) => paint_cover(&gp, tid, size, thumb_r),
                    None => {
                        let icon = match card.kind_label.as_str() {
                            "Sound" => crate::icons::Icon::Audio,
                            "Overlay" => crate::icons::Icon::Play,
                            _ if card.kind == "footage" => crate::icons::Icon::Footage,
                            _ => crate::icons::Icon::Comp,
                        };
                        let g = (thumb_r.height() * 0.3).clamp(10.0, 40.0 * s);
                        crate::icons::paint(&gp, Rect::from_center_size(thumb_r.center(), vec2(g, g)), icon, t.text_faint.gamma_multiply(0.7));
                    }
                }
                if card.needs_footage() {
                    let label = tr("needs footage");
                    let font = Tokens::medium(9.5 * s);
                    let bw = gp.layout_no_wrap(label.to_string(), font.clone(), Color32::BLACK).size().x + 12.0 * s;
                    let badge = Rect::from_min_size(pos2(thumb_r.min.x + 5.0 * s, thumb_r.max.y - 20.0 * s), vec2(bw, 15.0 * s));
                    if badge.max.x < thumb_r.max.x {
                        gp.rect_filled(badge, CornerRadius::same((7.0 * s) as u8), t.warning.gamma_multiply(0.9));
                        gp.text(badge.center(), Align2::CENTER_CENTER, label, font, Color32::BLACK);
                    }
                }
                if view.applying.as_deref() == Some(card.id.as_str()) {
                    gp.rect_filled(thumb_r, tr_radius, Color32::from_black_alpha(140));
                    gp.text(thumb_r.center(), Align2::CENTER_CENTER, tr("Downloading…"), Tokens::medium(11.5 * s), Color32::WHITE);
                }
                if selected {
                    gp.rect_stroke(tile, radius, Stroke::new(2.0, t.accent), egui::StrokeKind::Inside);
                }
                // Caption: the name, then what and where it is.
                let cx = tile.min.x + 7.0 * s;
                let cw = (tile.width() - 14.0 * s).max(4.0);
                let name_font = Tokens::medium(if compact { 10.0 } else { 11.0 } * s);
                if compact {
                    let mut job = egui::text::LayoutJob::simple(card.name.clone(), name_font, t.text, cw);
                    job.wrap.max_rows = 2;
                    let galley = gp.layout_job(job);
                    gp.galley(pos2(cx, thumb_r.max.y + 6.0 * s), galley, t.text);
                } else {
                    let name = elide(&gp, &card.name, &name_font, cw);
                    gp.text(pos2(cx, thumb_r.max.y + 13.0 * s), Align2::LEFT_CENTER, &name, name_font, t.text);
                    let meta_font = Tokens::ui(10.0 * s);
                    let meta = elide(&gp, &card.subtitle(), &meta_font, cw);
                    gp.text(pos2(cx, thumb_r.max.y + 28.0 * s), Align2::LEFT_CENTER, &meta, meta_font, t.text_faint);
                }
                let label = if card.needs_footage() { format!("{} (needs footage)", card.name) } else { card.name.clone() };
                app.auto.add(&format!("motionLibrary.card.{}", card.id), tile.intersect(grid), &label);
                if resp.clicked() {
                    view.selected = Some(card.id.clone());
                }
                if resp.double_clicked() {
                    apply_now = Some(card.id.clone());
                }
                if hovered {
                    let mut tip = format!("{}\n{}", card.name, card.subtitle());
                    if card.needs_footage() {
                        let names: Vec<&str> = card.placeholders.iter().map(|p| p.name.as_str()).take(3).collect();
                        tip.push_str(&format!("\nNeeds footage: {}", names.join(", ")));
                    }
                    tip.push_str("\nDouble-click to add");
                    resp.on_hover_text(tip);
                }
            }
        }
    }
    if hovered_card.is_none() {
        stop_preview(&ctx);
    }
    // Scroll bar.
    if max_scroll > 0.0 {
        let frac = grid.height() / content_h;
        let bar_h = (grid.height() * frac).max(24.0);
        let y = grid.min.y + (grid.height() - bar_h) * (view.scroll / max_scroll);
        gp.rect_filled(Rect::from_min_size(pos2(grid.max.x - 5.0, y), vec2(3.0, bar_h)), CornerRadius::same(2), t.text_faint.gamma_multiply(0.6));
    }

    // Footer: the selected card and Apply.
    let footer = Rect::from_min_max(pos2(rect.min.x, rect.max.y - footer_h), rect.max);
    p.line_segment([footer.left_top(), footer.right_top()], Stroke::new(1.0, t.separator));
    let sel = view.selected.as_ref().and_then(|id| client.catalog().and_then(|c| c.card(id).cloned()));
    let ar = Rect::from_min_size(pos2(footer.max.x - 80.0 * s, footer.center().y - 11.0 * s), vec2(72.0 * s, 22.0 * s));
    if let Some(card) = &sel {
        let info = format!("{}  ·  {}{}", card.name, card.subtitle(), if card.needs_footage() { "  ·  needs footage" } else { "" });
        draw_text_clipped(
            &p,
            Rect::from_min_max(pos2(footer.min.x + 10.0 * s, footer.min.y), pos2(ar.min.x - 8.0, footer.max.y)),
            &info,
            Tokens::ui(11.5 * s),
            t.text_dim,
        );
        if kit::button(app, ui, ar, "motionLibrary.apply", "Apply", true) {
            apply_now = Some(card.id.clone());
        }
    }
    if let Some(id) = apply_now {
        view.selected = Some(id.clone());
        view.applying = Some(id);
    }
    // Applying: poll until the downloads are in, then the import runs (one undo step).
    if let Some(id) = view.applying.clone() {
        match app.session.execute("library.apply", json!({"id": id, "wait": false})) {
            Ok(r) if r.get("pending").and_then(Value::as_bool) == Some(true) => {
                ctx.request_repaint_after(std::time::Duration::from_millis(100));
            }
            Ok(_) => view.applying = None,
            Err(e) => {
                app.ui.status = e.to_string();
                view.applying = None;
            }
        }
    }
    if view.display_open {
        display_popover(app, &ctx, display_r, &mut view, s);
    }
    view.clamp();
    app.ui.motion_library = view;
}

/// `motionLibrary.view`: change what the panel shows (`folder`, `query`, `scale`, `cardSize`,
/// `expand` / `collapse` folders, `display` popover); returns the view.
pub fn set_view(view: &mut ViewState, p: &Value) -> Result<Value, String> {
    if let Some(f) = p.get("folder") {
        let f = f.as_str().ok_or("`folder` is a folder path (\"\" = All motion)")?.trim().trim_matches('/');
        view.folder = f.to_string();
        view.reveal(f);
        view.scroll = 0.0;
    }
    if let Some(q) = p.get("query") {
        view.query = q.as_str().ok_or("`query` is text")?.to_string();
        view.scroll = 0.0;
    }
    if let Some(sc) = p.get("scale") {
        let sc = sc.as_f64().filter(|v| v.is_finite()).ok_or("`scale` is a percentage (80–140)")?;
        view.scale = ((sc / 10.0).round() * 10.0).clamp(SCALE_RANGE.0 as f64, SCALE_RANGE.1 as f64) as u32;
    }
    if let Some(c) = p.get("cardSize") {
        let c = c.as_f64().filter(|v| v.is_finite()).ok_or("`cardSize` is a width in points (88–1200)")?;
        view.card_size = (c as f32).clamp(CARD_SIZE_RANGE.0, CARD_SIZE_RANGE.1).round();
    }
    let paths = |v: &Value| -> Vec<String> {
        match v {
            Value::String(s) => vec![s.trim_matches('/').to_string()],
            Value::Array(a) => a.iter().filter_map(Value::as_str).map(|s| s.trim_matches('/').to_string()).collect(),
            _ => vec![],
        }
    };
    if let Some(e) = p.get("expand") {
        for f in paths(e) {
            view.reveal(&f);
            view.expanded.insert(f);
        }
    }
    if let Some(c) = p.get("collapse") {
        for f in paths(c) {
            view.expanded.remove(&f);
        }
    }
    if let Some(d) = p.get("display").and_then(Value::as_bool) {
        view.display_open = d;
    }
    view.clamp();
    Ok(view.to_json())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card(id: &str, cat: &str, size: Option<[u32; 2]>) -> Card {
        Card { id: id.into(), name: id.into(), category: cat.into(), thumb_size: size, ..Default::default() }
    }

    #[test]
    fn sections_rows_and_masonry() {
        let cards = vec![card("a", "A", Some([160, 100])), card("b", "A", Some([100, 200])), card("c", "A", None), card("d", "B", Some([100, 100]))];
        // 2 columns of 100: A's first row is as tall as its tallest card (b: 200 + caption 40).
        let l = layout(&cards, true, 210.0, 100.0, 10.0, 40.0, 30.0);
        assert_eq!(l.col_w, 100.0);
        let rects: Vec<(bool, f32, f32)> = l
            .items
            .iter()
            .map(|i| match i {
                Placed::Header { rect, .. } => (true, rect.min.x, rect.min.y),
                Placed::Card { rect, .. } => (false, rect.min.x, rect.min.y),
            })
            .collect();
        assert_eq!(rects, vec![(true, 0.0, 0.0), (false, 0.0, 40.0), (false, 110.0, 40.0), (false, 0.0, 290.0), (true, 0.0, 403.0), (false, 0.0, 443.0)]);
        assert!(matches!(&l.items[0], Placed::Header { count: 3, .. }));
        // Packed: d goes under a (the shorter column).
        let l = layout(&cards, false, 210.0, 100.0, 10.0, 40.0, 30.0);
        let ys: Vec<(f32, f32)> =
            l.items.iter().filter_map(|i| if let Placed::Card { rect, .. } = i { Some((rect.min.x, rect.min.y)) } else { None }).collect();
        assert_eq!(ys, vec![(0.0, 0.0), (110.0, 0.0), (0.0, 113.0), (0.0, 226.0)]);
        // A card wider than the grid takes the whole width.
        assert_eq!(layout(&cards, false, 80.0, 1200.0, 10.0, 40.0, 30.0).col_w, 80.0);
    }

    #[test]
    fn view_command_clamps_and_reveals() {
        let mut v = ViewState::default();
        let r = set_view(&mut v, &json!({"scale": 175, "cardSize": 20, "folder": "Devin Jatho/Instagram/Reels"})).unwrap();
        assert_eq!((v.scale, v.card_size), (140, 88.0));
        assert!(v.expanded.contains("Devin Jatho") && v.expanded.contains("Devin Jatho/Instagram"));
        assert_eq!(r["folder"], "Devin Jatho/Instagram/Reels");
        set_view(&mut v, &json!({"scale": 84, "collapse": "Devin Jatho", "cardSize": 400.4})).unwrap();
        assert_eq!((v.scale, v.card_size), (80, 400.0));
        assert!(!v.expanded.contains("Devin Jatho"));
        assert!(set_view(&mut v, &json!({"scale": "big"})).is_err());
        let mut odd = ViewState { scale: 0, card_size: f32::NAN, ..Default::default() };
        odd.clamp();
        assert_eq!((odd.scale, odd.card_size), (80, DEFAULT_CARD_SIZE));
        // An old workspace without the new settings.
        let old: ViewState = serde_json::from_value(json!({"query": "x"})).unwrap();
        assert_eq!((old.scale, old.card_size), (100, DEFAULT_CARD_SIZE));
    }
}
