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

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use effectcraft_engine::motionlib::{BlobRef, BlobState, Card, Client};
use egui::{Align2, Color32, CornerRadius, Rect, Sense, Stroke, pos2, vec2};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::panel_kit as kit;
use crate::i18n::tr;
use crate::theme::Tokens;
use crate::{EffectcraftApp, widgets};

/// What the panel shows (kept with the workspace).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ViewState {
    pub query: String,
    /// Folder path (`""` = all).
    pub folder: String,
    /// The selected card's id.
    pub selected: Option<String>,
    /// Vertical scroll of the cards (points).
    pub scroll: f32,
    /// The card being applied (downloading) and when it started.
    #[serde(skip)]
    pub applying: Option<String>,
}

const TILE_MIN_W: f32 = 150.0;
const GAP: f32 = 8.0;
const NAME_H: f32 = 20.0;
/// Thumbnails kept as textures; the least recently shown go first.
const MAX_THUMBS: usize = 160;
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
fn thumbnail(ctx: &egui::Context, client: &Client, blob: &BlobRef, budget: &mut u32) -> Thumb {
    let id = egui::Id::new(("ml-thumb", &blob.hash));
    if let Some(t) = ctx.data(|d| d.get_temp::<Thumb>(id)) {
        crate::thumb_lru::touch::<Thumb>(ctx, "ml-thumb", id, MAX_THUMBS);
        return t;
    }
    let jid = egui::Id::new(("ml-thumb-job", &blob.hash));
    if let Some(job) = ctx.data(|d| d.get_temp::<ThumbJob>(jid)) {
        let done = job.get()?;
        let thumb = done.as_ref().map(|([w, h], px)| {
            let img = egui::ColorImage::from_rgba_unmultiplied([*w as usize, *h as usize], px);
            (ctx.load_texture(format!("ml-{}", blob.hash), img, egui::TextureOptions::LINEAR), [*w, *h])
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
        let _ = job.set(decode_image(&bytes, PREVIEW_SIDE));
        ctx.request_repaint();
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        THUMB_JOBS.fetch_add(1, Ordering::Relaxed);
        let slot = ThumbSlot;
        let ctx2 = ctx.clone();
        let spawned = std::thread::Builder::new().name("ec-library-thumb".into()).spawn(move || {
            let _slot = slot;
            let _ = job.set(decode_image(&bytes, PREVIEW_SIDE));
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

/// Cards matching the view, cached per catalog generation, query and folder.
fn visible_cards(ctx: &egui::Context, client: &Client, view: &ViewState) -> Arc<Vec<Card>> {
    type Cached = (u64, String, String, Arc<Vec<Card>>);
    let id = egui::Id::new("ml-cards");
    // The adopted catalog (a new one after an update), by identity.
    let generation = client.catalog().map_or(0, |c| Arc::as_ptr(&c) as usize as u64);
    if let Some((g, q, f, cards)) = ctx.data(|d| d.get_temp::<Cached>(id))
        && g == generation
        && q == view.query
        && f == view.folder
    {
        return cards;
    }
    let cards: Arc<Vec<Card>> = Arc::new(
        client
            .catalog()
            .map(|cat| effectcraft_engine::motionlib::search(&cat.cards, &view.query, &view.folder).into_iter().cloned().collect())
            .unwrap_or_default(),
    );
    ctx.data_mut(|d| d.insert_temp::<Cached>(id, (generation, view.query.clone(), view.folder.clone(), cards.clone())));
    cards
}

fn draw_text_clipped(p: &egui::Painter, r: Rect, text: &str, font: egui::FontId, color: Color32) {
    let galley = p.layout_no_wrap(text.to_string(), font, color);
    let pos = pos2(r.min.x, r.center().y - galley.size().y / 2.0);
    p.with_clip_rect(r.intersect(p.clip_rect())).galley(pos, galley, color);
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

    // Toolbar: search, refresh.
    let top = rect.min.y + 8.0;
    let sr = Rect::from_min_size(pos2(rect.min.x + 8.0, top), vec2((rect.width() - 52.0).max(60.0), 24.0));
    let mut q = view.query.clone();
    let resp = widgets::search_field(ui, sr, &mut q, "Search cards", &t);
    app.auto.add_response("motionLibrary.search", &resp, "Search cards");
    if q != view.query {
        view.query = q;
        view.scroll = 0.0;
    }
    let rr = Rect::from_min_size(pos2(sr.max.x + 6.0, top), vec2(30.0, 24.0));
    if kit::button(app, ui, rr, "motionLibrary.refresh", if status.refreshing { "…" } else { "↻" }, false) {
        kit::exec(app, "library.sync", json!({"wait": false}));
    }

    // Second row: the update chip, or the status.
    let row = Rect::from_min_size(pos2(rect.min.x + 8.0, top + 30.0), vec2(rect.width() - 16.0, 20.0));
    if let Some(u) = &status.update {
        let w = (u.label.chars().count() as f32 * 6.3 + 34.0).min(row.width());
        let chip = Rect::from_min_size(row.min, vec2(w, 20.0));
        let resp = ui.interact(chip, egui::Id::new("ml-update-chip"), Sense::click());
        let fill = if resp.hovered() { t.accent_hover } else { t.accent };
        p.rect_filled(chip, CornerRadius::same(10), fill);
        p.text(pos2(chip.min.x + 10.0, chip.center().y), Align2::LEFT_CENTER, u.label.as_str(), Tokens::medium(11.5), Color32::WHITE);
        app.auto.add("motionLibrary.updateChip", chip, &u.label);
        if resp.on_hover_text(tr("Switch to the newest library")).clicked() {
            kit::exec(app, "library.update", json!({}));
            view.scroll = 0.0;
        }
    } else {
        let (text, color) = match (&status.error, status.cards) {
            (Some(e), _) => (e.clone(), t.warning),
            (None, 0) if status.refreshing || !status.checked => (tr("Loading the library…").to_string(), t.text_dim),
            (None, n) => (
                format!(
                    "{n} cards · version {}{}",
                    status.version,
                    if status.downloading > 0 { format!(" · downloading {}", status.downloading) } else { String::new() }
                ),
                t.text_faint,
            ),
        };
        draw_text_clipped(&p, row, &text, Tokens::ui(11.0), color);
        app.auto.add("motionLibrary.status", row, &text);
    }

    // Folders: a list on the left, or a dropdown when the panel is narrow.
    let mut body_top = top + 56.0;
    let footer_h = 30.0;
    let folders = client.catalog().map(|c| c.folder_counts()).unwrap_or_default();
    let total = client.catalog().map_or(0, |c| c.cards.len());
    let all: Vec<(String, usize)> = std::iter::once((String::new(), total)).chain(folders).collect();
    let folder_name = |f: &str| if f.is_empty() { tr("All cards").to_string() } else { f.to_string() };
    let narrow = rect.width() < 460.0;
    let mut grid_left = rect.min.x;
    if narrow {
        let dr = Rect::from_min_size(pos2(rect.min.x + 8.0, body_top), vec2((rect.width() - 16.0).max(60.0), 22.0));
        let labels: Vec<String> = all.iter().map(|(f, n)| format!("{}  ({n})", folder_name(f))).collect();
        let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
        let cur = all.iter().position(|(f, _)| *f == view.folder).unwrap_or(0);
        if let Some(i) = kit::dropdown(app, ui, dr, "motionLibrary.folder", &refs, cur)
            && let Some((f, _)) = all.get(i)
        {
            view.folder = f.clone();
            view.scroll = 0.0;
        }
        body_top += 28.0;
    } else {
        let side_w = 170.0_f32.min(rect.width() * 0.32);
        let side = Rect::from_min_max(pos2(rect.min.x, body_top), pos2(rect.min.x + side_w, rect.max.y - footer_h));
        p.line_segment([side.right_top(), side.right_bottom()], Stroke::new(1.0, t.separator));
        let sp = p.with_clip_rect(side);
        let mut fy = side.min.y;
        for (i, (folder, n)) in all.iter().enumerate() {
            let r = Rect::from_min_size(pos2(side.min.x, fy), vec2(side.width(), 20.0));
            fy += 20.0;
            if r.min.y > side.max.y {
                break;
            }
            let depth = if folder.is_empty() { 0 } else { folder.matches('/').count() };
            let name = if folder.is_empty() { tr("All cards").to_string() } else { folder.rsplit('/').next().unwrap_or(folder).to_string() };
            let resp = ui.interact(r.intersect(side), egui::Id::new(("ml-folder", i)), Sense::click());
            if view.folder == *folder {
                sp.rect_filled(r, 0.0, t.row_selected);
            } else if resp.hovered() {
                sp.rect_filled(r, 0.0, t.hover);
            }
            let x = r.min.x + 10.0 + depth as f32 * 12.0;
            draw_text_clipped(&sp, Rect::from_min_max(pos2(x, r.min.y), pos2(r.max.x - 34.0, r.max.y)), &name, Tokens::ui(12.0), t.text);
            sp.text(pos2(r.max.x - 8.0, r.center().y), Align2::RIGHT_CENTER, n.to_string(), Tokens::ui(10.5), t.text_faint);
            app.auto.add(&format!("motionLibrary.folder.{}", if folder.is_empty() { "all" } else { folder }), r, &name);
            if resp.clicked() {
                view.folder = folder.clone();
                view.scroll = 0.0;
            }
        }
        grid_left = side.max.x + 1.0;
    }

    // Cards.
    let grid = Rect::from_min_max(pos2(grid_left, body_top), pos2(rect.max.x, rect.max.y - footer_h));
    let gp = p.with_clip_rect(grid);
    let cards = visible_cards(&ctx, &client, &view);
    let inner_w = (grid.width() - GAP * 2.0).max(40.0);
    let cols = ((inner_w + GAP) / (TILE_MIN_W + GAP)).floor().max(1.0) as usize;
    let tile_w = (inner_w - GAP * (cols as f32 - 1.0)) / cols as f32;
    let thumb_h = (tile_w * 9.0 / 16.0).round();
    let tile_h = thumb_h + NAME_H;
    let rows = cards.len().div_ceil(cols);
    let content_h = rows as f32 * (tile_h + GAP) + GAP;
    let max_scroll = (content_h - grid.height()).max(0.0);
    if ui.rect_contains_pointer(grid) {
        let dy = ctx.input(|i| i.smooth_scroll_delta.y);
        if dy.is_finite() && dy != 0.0 {
            view.scroll -= dy;
        }
    }
    view.scroll = view.scroll.clamp(0.0, max_scroll);
    if cards.is_empty() {
        let msg = if total == 0 { "" } else { "No cards match" };
        gp.text(grid.center(), Align2::CENTER_CENTER, tr(msg), Tokens::ui(12.0), t.text_faint);
    }
    let first_row = ((view.scroll - GAP) / (tile_h + GAP)).floor().max(0.0) as usize;
    let last_row = (((view.scroll + grid.height()) / (tile_h + GAP)).ceil() as usize).min(rows);
    let mut budget = 6u32;
    let mut hovered_card = None;
    let mut apply_now: Option<String> = None;
    for row in first_row..last_row {
        for col in 0..cols {
            let Some(card) = cards.get(row * cols + col) else { break };
            let x = grid.min.x + GAP + col as f32 * (tile_w + GAP);
            let y = grid.min.y + GAP + row as f32 * (tile_h + GAP) - view.scroll;
            let tile = Rect::from_min_size(pos2(x, y), vec2(tile_w, tile_h));
            let thumb_r = Rect::from_min_size(tile.min, vec2(tile_w, thumb_h));
            let resp = ui.interact(tile.intersect(grid), egui::Id::new(("ml-card", &card.id)), Sense::click());
            let selected = view.selected.as_deref() == Some(card.id.as_str());
            gp.rect_filled(thumb_r, CornerRadius::same(4), t.field_bg);
            let hovered = resp.hovered();
            let live = if hovered { preview_frame(app, &ctx, &client, card) } else { None };
            if hovered {
                hovered_card = Some(card.id.clone());
            }
            let tex = live.map(|t| (t.id(), t.size_vec2())).or_else(|| {
                let thumb = card.thumbnail.as_ref().and_then(|b| thumbnail(&ctx, &client, b, &mut budget));
                thumb.map(|(tex, [w, h])| (tex.id(), vec2(w as f32, h as f32)))
            });
            if let Some((tid, size)) = tex {
                let s = (thumb_r.width() / size.x.max(1.0)).min(thumb_r.height() / size.y.max(1.0));
                let r = Rect::from_center_size(thumb_r.center(), size * s);
                gp.image(tid, r, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
            } else if card.thumbnail.is_some() {
                gp.text(thumb_r.center(), Align2::CENTER_CENTER, "…", Tokens::ui(14.0), t.text_faint);
            }
            if card.needs_footage() {
                let label = tr("needs footage");
                let bw = label.chars().count() as f32 * 5.8 + 12.0;
                let badge = Rect::from_min_size(pos2(thumb_r.min.x + 5.0, thumb_r.max.y - 21.0), vec2(bw, 16.0));
                gp.rect_filled(badge, CornerRadius::same(8), t.warning.gamma_multiply(0.9));
                gp.text(badge.center(), Align2::CENTER_CENTER, label, Tokens::medium(10.0), Color32::BLACK);
            }
            if view.applying.as_deref() == Some(card.id.as_str()) {
                gp.rect_filled(thumb_r, CornerRadius::same(4), Color32::from_black_alpha(140));
                gp.text(thumb_r.center(), Align2::CENTER_CENTER, tr("Downloading…"), Tokens::medium(11.5), Color32::WHITE);
            }
            if selected {
                gp.rect_stroke(thumb_r, CornerRadius::same(4), Stroke::new(2.0, t.accent), egui::StrokeKind::Inside);
            } else if hovered {
                gp.rect_stroke(thumb_r, CornerRadius::same(4), Stroke::new(1.0, t.text_faint), egui::StrokeKind::Inside);
            }
            let name_r = Rect::from_min_size(pos2(tile.min.x + 2.0, thumb_r.max.y), vec2(tile_w - 4.0, NAME_H));
            draw_text_clipped(&gp, name_r, &card.name, Tokens::ui(11.5), if selected { t.text } else { t.text_dim });
            let label = if card.needs_footage() { format!("{} (needs footage)", card.name) } else { card.name.clone() };
            app.auto.add(&format!("motionLibrary.card.{}", card.id), tile.intersect(grid), &label);
            if resp.clicked() {
                view.selected = Some(card.id.clone());
            }
            if resp.double_clicked() {
                apply_now = Some(card.id.clone());
            }
            if hovered {
                let mut tip = card.category.clone();
                if card.needs_footage() {
                    let names: Vec<&str> = card.placeholders.iter().map(|p| p.name.as_str()).take(3).collect();
                    tip.push_str(&format!("\nNeeds footage: {}", names.join(", ")));
                }
                resp.on_hover_text(tip);
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
    let ar = Rect::from_min_size(pos2(footer.max.x - 78.0, footer.min.y + 5.0), vec2(70.0, 20.0));
    if let Some(card) = &sel {
        let info = format!("{}  ·  {}{}", card.name, card.category, if card.needs_footage() { "  ·  needs footage" } else { "" });
        draw_text_clipped(
            &p,
            Rect::from_min_max(pos2(footer.min.x + 10.0, footer.min.y), pos2(ar.min.x - 8.0, footer.max.y)),
            &info,
            Tokens::ui(11.5),
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
    app.ui.motion_library = view;
}
