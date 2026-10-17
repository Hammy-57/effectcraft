//! Publishing (the Mac that manages the library): read a library folder's `library.json`, hash
//! every file the chosen folders' cards need, upload only blobs the remote doesn't have yet,
//! then the manifest and a new `head.json` with the version bumped.
//!
//! Big footage stays home: video (unless `include_video`) and files over `max_file_bytes` become
//! [`Placeholder`]s; the card's project still names them, so applying it imports them as missing
//! footage to relink.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::hash::sha256_hex;
use crate::manifest::{BlobRef, Card, CardFile, HEAD_KEY, Head, Manifest, Placeholder, decode_blob, encode_blob};
use crate::remote::{Remote, get_blocking};

/// Ticks per second (`effectcraft_time::Tick`), to report placeholder durations in seconds.
const TICKS_PER_SECOND: f64 = 254_016_000_000.0;
const TIMEOUT: Duration = Duration::from_secs(120);
/// The hash cache next to `library.json` (path, size and mtime → SHA-256).
const HASH_CACHE: &str = ".motion-library-hashes.json";

/// Video containers left out unless `include_video`.
const VIDEO: &[&str] = &["mov", "mp4", "m4v", "mkv", "webm", "avi", "mxf", "mpg", "mpeg", "mts", "m2ts", "wmv", "flv", "3gp", "ogv", "r3d", "braw"];

#[derive(Clone, Debug)]
pub struct Options {
    /// The folder with `library.json` (e.g. `~/Documents/Elias Motion Library/effectcraft`).
    pub library: PathBuf,
    /// Folders to publish (a card is in when its category is one of them or below one).
    pub folders: Vec<String>,
    /// Files bigger than this stay home as placeholders.
    pub max_file_bytes: u64,
    pub include_video: bool,
    /// Work everything out, upload nothing.
    pub dry_run: bool,
    /// Upload every blob again (a remote that lost files).
    pub force: bool,
}

impl Options {
    pub fn new(library: impl Into<PathBuf>, folders: Vec<String>) -> Options {
        Options { library: library.into(), folders, max_file_bytes: 100 * 1024 * 1024, include_video: false, dry_run: false, force: false }
    }
}

/// A file left out of the upload, for the report.
#[derive(Clone, Debug, Serialize)]
pub struct Skipped {
    pub card: String,
    pub path: String,
    pub size: u64,
    pub reason: String,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Report {
    pub version: u64,
    pub previous: u64,
    /// Nothing changed: the version stays.
    pub unchanged: bool,
    #[serde(rename = "dryRun")]
    pub dry_run: bool,
    pub cards: usize,
    #[serde(rename = "needsFootage")]
    pub needs_footage: usize,
    /// Blobs uploaded (or to upload, dry run) and their stored bytes.
    pub uploads: usize,
    #[serde(rename = "uploadBytes")]
    pub upload_bytes: u64,
    /// Every blob the catalog references and their stored bytes (what the bucket holds).
    pub blobs: usize,
    #[serde(rename = "totalBytes")]
    pub total_bytes: u64,
    /// The cards changed relative to the previous catalog.
    pub new: usize,
    pub changed: usize,
    pub removed: usize,
    pub skipped: Vec<Skipped>,
    /// Entries that could not be published (missing project, unreadable file), with why.
    pub errors: Vec<String>,
}

#[derive(Clone, Default, Serialize, Deserialize)]
struct HashEntry {
    size: u64,
    mtime: u128,
    hash: String,
}

/// Hashes of files by path, size and modification time: a re-publish only reads changed files.
struct Hasher {
    file: PathBuf,
    cache: BTreeMap<String, HashEntry>,
    dirty: bool,
}

impl Hasher {
    fn open(library: &Path) -> Hasher {
        let file = library.join(HASH_CACHE);
        let cache = std::fs::read(&file).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        Hasher { file, cache, dirty: false }
    }
    fn stat(path: &Path) -> Option<(u64, u128)> {
        let m = std::fs::metadata(path).ok().filter(|m| m.is_file())?;
        let t = m.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_nanos();
        Some((m.len(), t))
    }
    /// The file's hash and size (`None`: not a readable file).
    fn hash(&mut self, path: &Path) -> Option<(String, u64)> {
        let (size, mtime) = Self::stat(path)?;
        let k = path.to_string_lossy().to_string();
        if let Some(e) = self.cache.get(&k).filter(|e| e.size == size && e.mtime == mtime) {
            return Some((e.hash.clone(), size));
        }
        let hash = sha256_file(path)?;
        self.cache.insert(k, HashEntry { size, mtime, hash: hash.clone() });
        self.dirty = true;
        Some((hash, size))
    }
    fn save(&self) {
        if self.dirty
            && let Ok(b) = serde_json::to_vec(&self.cache)
        {
            let _ = crate::store::write_atomic(&self.file, &b);
        }
    }
}

fn sha256_file(path: &Path) -> Option<String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let mut f = std::fs::File::open(path).ok()?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf).ok()?;
        if n == 0 {
            break;
        }
        h.update(buf.get(..n)?);
    }
    Some(crate::hash::hex(&h.finalize()))
}

fn ext(path: &str) -> String {
    Path::new(path).extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default()
}

fn file_name(path: &str) -> String {
    Path::new(path).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()
}

/// Whether text files should be stored gzipped.
fn gz_for(path: &str) -> bool {
    matches!(ext(path).as_str(), "ecproj" | "json" | "svg" | "txt" | "jsx" | "csv")
}

/// Every file path a project names (`"path"` strings), with the footage item around it.
fn project_paths(v: &Value) -> Vec<(String, Option<&serde_json::Map<String, Value>>)> {
    fn walk<'a>(v: &'a Value, depth: usize, out: &mut Vec<(String, Option<&'a serde_json::Map<String, Value>>)>) {
        if depth > 256 {
            return;
        }
        match v {
            Value::Object(map) => {
                if let Some(Value::String(p)) = map.get("path")
                    && (p.starts_with('/') || p.get(1..3) == Some(":\\"))
                {
                    let footage = (map.get("type").and_then(Value::as_str) == Some("Footage")).then_some(map);
                    out.push((p.clone(), footage));
                }
                map.values().for_each(|x| walk(x, depth + 1, out));
            }
            Value::Array(items) => items.iter().for_each(|x| walk(x, depth + 1, out)),
            _ => {}
        }
    }
    let mut out = Vec::new();
    walk(v, 0, &mut out);
    let mut seen = BTreeSet::new();
    out.retain(|(p, _)| seen.insert(p.clone()));
    out
}

fn text(e: &Value, key: &str) -> String {
    e.get(key).and_then(Value::as_str).unwrap_or_default().to_string()
}

fn strings(e: &Value, key: &str) -> Vec<String> {
    e.get(key)
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| match v {
                    Value::String(s) => Some(s.clone()),
                    Value::Object(o) => o.get("description").and_then(Value::as_str).map(str::to_string),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default()
}

/// A blob to upload: its reference and where its bytes come from.
struct Upload {
    blob: BlobRef,
    source: PathBuf,
}

struct Builder<'a> {
    opts: &'a Options,
    hasher: Hasher,
    uploads: BTreeMap<String, Upload>,
    report: Report,
}

impl Builder<'_> {
    fn resolve(&self, path: &str) -> PathBuf {
        let p = Path::new(path);
        if p.is_absolute() { p.to_path_buf() } else { self.opts.library.join(p) }
    }

    /// A blob for the file at `path` (queued for upload); `None` when it can't be read.
    fn blob(&mut self, path: &Path, name: &str) -> Option<BlobRef> {
        let (hash, size) = self.hasher.hash(path)?;
        let blob = BlobRef { hash, size, gz: gz_for(name), name: name.to_string() };
        self.uploads.entry(blob.remote_key()).or_insert_with(|| Upload { blob: blob.clone(), source: path.to_path_buf() });
        Some(blob)
    }

    fn card(&mut self, e: &Value) -> Result<Card, String> {
        let id = text(e, "id");
        let name = text(e, "name");
        let kind = text(e, "kind");
        let file = text(e, "file");
        if file.is_empty() {
            return Err(format!("{name}: no file"));
        }
        let project_path = self.resolve(&file);
        let mut card = Card {
            id: id.clone(),
            name: name.clone(),
            category: text(e, "category"),
            kind: kind.clone(),
            comp: text(e, "comp"),
            description: text(e, "description"),
            tags: strings(e, "tags"),
            aliases: strings(e, "aliases"),
            visual_caption: text(e, "visual_caption"),
            motion_events: strings(e, "motion_events"),
            summary: text(e, "summary"),
            generated_summary: text(e, "generated_summary"),
            motion_terms: text(e, "motion_terms"),
            priority: e.get("priority").and_then(Value::as_bool).unwrap_or(false),
            duration: e.get("duration").and_then(Value::as_f64).filter(|d| d.is_finite()),
            ..Card::default()
        };
        let max = self.opts.max_file_bytes;
        for (key, slot) in [("thumbnail", 0), ("preview", 1)] {
            let p = text(e, key);
            if p.is_empty() {
                continue;
            }
            let path = self.resolve(&p);
            if Hasher::stat(&path).is_some_and(|(size, _)| size <= max) {
                let b = self.blob(&path, &file_name(&p));
                if slot == 0 { card.thumbnail = b } else { card.preview = b }
            }
        }
        let project_name = file_name(&file);
        card.project = Some(self.blob(&project_path, &project_name).ok_or_else(|| format!("{name}: cannot read {}", project_path.display()))?);
        if kind == "comp" {
            let bytes = std::fs::read(&project_path).map_err(|e| format!("{name}: {}: {e}", project_path.display()))?;
            let v: Value = serde_json::from_slice(&bytes).map_err(|e| format!("{name}: {}: {e}", project_path.display()))?;
            for (path, footage) in project_paths(&v) {
                let stat = Hasher::stat(Path::new(&path));
                let size = stat.map_or(0, |s| s.0);
                let reason = match stat {
                    None => Some("missing"),
                    Some(_) if !self.opts.include_video && VIDEO.contains(&ext(&path).as_str()) => Some("video"),
                    Some((size, _)) if size > max => Some("too large"),
                    Some(_) => None,
                };
                let blob = if reason.is_none() { self.blob(Path::new(&path), &file_name(&path)) } else { None };
                match blob {
                    Some(blob) => card.files.push(CardFile { path, blob }),
                    None => {
                        let reason = reason.unwrap_or("unreadable").to_string();
                        let num = |k: &str| footage.and_then(|f| f.get(k)).and_then(Value::as_f64);
                        let rate = footage.and_then(|f| f.get("frame_rate")).and_then(|r| Some(r.get("num")?.as_f64()? / r.get("den")?.as_f64()?.max(1.0)));
                        self.report.skipped.push(Skipped { card: name.clone(), path: path.clone(), size, reason: reason.clone() });
                        card.placeholders.push(Placeholder {
                            name: file_name(&path),
                            path,
                            size,
                            reason,
                            width: num("width").map(|w| w as u32).filter(|w| *w > 1),
                            height: num("height").map(|h| h as u32).filter(|h| *h > 1),
                            duration: num("duration").map(|d| d / TICKS_PER_SECOND),
                            fps: rate.filter(|r| r.is_finite() && *r > 0.0),
                        });
                    }
                }
            }
        }
        card.rev = card.compute_rev();
        Ok(card)
    }
}

/// The catalog published at `remote` now (head and manifest), if any.
pub fn current(remote: &dyn Remote) -> Result<Option<(Head, Manifest)>, String> {
    let Some(raw) = get_blocking(remote, HEAD_KEY, TIMEOUT)? else { return Ok(None) };
    let head = Head::parse(&raw)?;
    let stored = get_blocking(remote, &head.manifest.remote_key(), TIMEOUT)?.ok_or("the published head names a manifest that isn't there")?;
    let m = Manifest::parse(&decode_blob(&head.manifest, &stored)?)?;
    Ok(Some((head, m)))
}

/// Publish `opts.folders` of the library at `opts.library` to `remote`. `progress` gets a line
/// now and then (hashing, uploads).
pub fn publish(remote: &dyn Remote, opts: &Options, progress: &mut dyn FnMut(String)) -> Result<Report, String> {
    let index_path = opts.library.join("library.json");
    let index: Value = serde_json::from_slice(&std::fs::read(&index_path).map_err(|e| format!("{}: {e}", index_path.display()))?)
        .map_err(|e| format!("{}: {e}", index_path.display()))?;
    let lib_name = index.get("name").and_then(Value::as_str).unwrap_or("Motion Library").to_string();
    let folders: Vec<String> = opts.folders.iter().map(|f| f.trim().trim_matches('/').to_string()).filter(|f| !f.is_empty()).collect();
    if folders.is_empty() {
        return Err("no folders to publish".into());
    }
    let entries: Vec<&Value> = index
        .get("entries")
        .and_then(Value::as_array)
        .map(|a| a.iter().collect::<Vec<_>>())
        .unwrap_or_default()
        .into_iter()
        .filter(|e| matches!(e.get("kind").and_then(Value::as_str), Some("comp" | "footage")))
        .filter(|e| {
            let cat = text(e, "category");
            folders.iter().any(|f| cat == *f || cat.strip_prefix(f.as_str()).is_some_and(|r| r.starts_with('/')))
        })
        .collect();
    progress(format!("{} cards in {}", entries.len(), folders.join(", ")));

    let previous = current(remote)?;
    let mut b = Builder { opts, hasher: Hasher::open(&opts.library), uploads: BTreeMap::new(), report: Report { dry_run: opts.dry_run, ..Report::default() } };
    let mut cards = Vec::new();
    for (i, e) in entries.iter().enumerate() {
        if i % 50 == 0 && i > 0 {
            progress(format!("read {i} of {} cards", entries.len()));
        }
        match b.card(e) {
            Ok(c) => cards.push(c),
            Err(err) => b.report.errors.push(err),
        }
    }
    b.hasher.save();
    let mut seen = BTreeSet::new();
    cards.retain(|c| seen.insert(c.id.clone()));

    let prev_version = previous.as_ref().map_or(0, |(h, _)| h.version);
    let mut report = std::mem::take(&mut b.report);
    report.previous = prev_version;
    report.cards = cards.len();
    report.needs_footage = cards.iter().filter(|c| c.needs_footage()).count();
    let prev_cards: Vec<(String, String)> = previous.as_ref().map(|(_, m)| m.cards.iter().map(|c| (c.id.clone(), c.rev.clone())).collect()).unwrap_or_default();
    let now_cards: Vec<(String, String)> = cards.iter().map(|c| (c.id.clone(), c.rev.clone())).collect();
    let same_folders = previous.as_ref().is_some_and(|(_, m)| m.folders == folders && m.name == lib_name);
    report.unchanged = previous.is_some() && prev_cards == now_cards && same_folders && !opts.force;
    let version = if report.unchanged { prev_version } else { prev_version + 1 };
    report.version = version;
    let manifest = Manifest::new(&lib_name, folders, cards, version);
    if let Some((_, m)) = &previous {
        let ch = m.changes_to(&manifest);
        (report.new, report.changed, report.removed) = (ch.new, ch.changed, ch.removed);
    } else {
        report.new = manifest.cards.len();
    }

    // What the remote has: every blob the published catalog references.
    let have: BTreeSet<String> = previous
        .as_ref()
        .map(|(h, m)| {
            m.cards
                .iter()
                .flat_map(|c| c.thumbnail.iter().chain(c.preview.iter()).chain(c.project.iter()).chain(c.files.iter().map(|f| &f.blob)))
                .map(BlobRef::remote_key)
                .chain(std::iter::once(h.manifest.remote_key()))
                .collect()
        })
        .unwrap_or_default();
    report.blobs = b.uploads.len();
    let todo: Vec<&Upload> = b.uploads.values().filter(|u| opts.force || !have.contains(&u.blob.remote_key())).collect();
    report.uploads = todo.len();
    for u in b.uploads.values() {
        report.total_bytes += u.blob.size;
    }
    for u in &todo {
        report.upload_bytes += u.blob.size;
    }
    if opts.dry_run || report.unchanged {
        return Ok(report);
    }
    let mut sent = 0u64;
    for (i, u) in todo.iter().enumerate() {
        let data = std::fs::read(&u.source).map_err(|e| format!("{}: {e}", u.source.display()))?;
        if sha256_hex(&data) != u.blob.hash {
            return Err(format!("{} changed while publishing; publish again", u.source.display()));
        }
        let stored = encode_blob(&data, u.blob.gz)?;
        remote.put(&u.blob.remote_key(), &stored)?;
        sent += u.blob.size;
        if i % 25 == 0 || i + 1 == todo.len() {
            progress(format!("uploaded {} of {} ({:.1} of {:.1} MB)", i + 1, todo.len(), sent as f64 / 1e6, report.upload_bytes as f64 / 1e6));
        }
    }
    // The manifest, then the head that points at it (clients never see a half-published catalog).
    let raw = serde_json::to_vec(&manifest).map_err(|e| e.to_string())?;
    let mblob = BlobRef { hash: sha256_hex(&raw), size: raw.len() as u64, gz: true, name: String::new() };
    remote.put(&mblob.remote_key(), &encode_blob(&raw, true)?)?;
    let published = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let head = Head::new(version, mblob, published, manifest.cards.len());
    remote.put(HEAD_KEY, &serde_json::to_vec_pretty(&head).map_err(|e| e.to_string())?)?;
    progress(format!("published version {version}: {} cards", manifest.cards.len()));
    Ok(report)
}
