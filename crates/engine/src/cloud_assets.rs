//! Cloud mirror (CR7): everything a project reads from disk, so a rented render machine can hold
//! the same files at the same absolute paths, and a check on that machine that nothing is missing
//! before it renders a frame for the Mac.
//!
//! - `project.dependencies` (on the Mac): the manifest. Files (footage, image sequence frames,
//!   proxies, effect files such as LUTs, 3D model sidecars, WebAssembly plug-ins) and fonts (the
//!   face each text style resolves to here, with its file), each with size, mtime and sha256.
//! - `cloud.preflight` (on the server): checks a manifest, or the server's own project, against
//!   this machine. Missing footage renders colour bars and a missing font renders Inter, both
//!   silently, so the cloud preview is refused while anything is missing, and the reply says what.
//! - `cloud.pendingUploads` / `cloud.markUploaded` (on the Mac): files the project uses that the
//!   mirror has not confirmed on the server yet (a new import), so frames render locally until
//!   they are there.
//!
//! [`blockers`] is the same check without a manifest, cheap enough to run per project revision:
//! the frame server refuses frames with it.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;

use effectcraft_keyframe::text_doc::FontFileLink;
use effectcraft_project::{FootageKind, GroupKind, ItemId, ItemKind, Project};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::commands::{CommandSpec, bad, str_p};
use crate::{EngineError, Result, Session, query};

/// Files up to this size are hashed by default (fonts, LUTs, stills); larger ones only with
/// `hash: "all"` (the mirror hashes them itself, once, when it has to compare).
pub const SMALL_FILE: u64 = 64 * 1024 * 1024;
/// Files the mirror lists first, so a long upload is known up front.
pub const BIG_FILE: u64 = 1024 * 1024 * 1024;
/// At most this many files in a model's sidecar list (a malformed model can't explode it).
const MAX_SIDECARS: usize = 4096;

/// What a project needs from disk.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    pub version: u32,
    #[serde(default)]
    pub host: HostInfo,
    #[serde(default)]
    pub files: Vec<FileEntry>,
    #[serde(default)]
    pub fonts: Vec<FontEntry>,
    /// Effect ids the project applies (built-in or plug-in).
    #[serde(default)]
    pub effects: Vec<String>,
    /// Expressions whose value depends on the machine or the clock (`Math.random`, `Date`):
    /// a cloud frame can differ from the Mac's for them. Informational.
    #[serde(default)]
    pub nondeterministic: Vec<Value>,
    #[serde(default)]
    pub totals: Totals,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct HostInfo {
    pub os: String,
    /// The CJK locale tag in force (`EFFECTCRAFT_CJK_LOCALE` pins it on the server).
    pub cjk_locale: String,
    /// The Han glyph forms it selects: `ja`, `ko`, `zh-hans`, `zh-hant`.
    pub cjk_forms: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Totals {
    pub files: usize,
    pub bytes: u64,
    pub fonts: usize,
    /// Files present on the source machine but missing there (they render as colour bars on
    /// both machines; reported, never uploaded).
    pub missing_here: usize,
}

/// One file the project reads, at its absolute path.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FileEntry {
    pub path: String,
    /// `footage`, `sequence`, `proxy`, `effect`, `model`, `plugin`, `font` (a linked font file,
    /// verified at its own path).
    pub kind: String,
    #[serde(default)]
    pub size: u64,
    /// Modification time, whole seconds since the Unix epoch (what tar keeps).
    #[serde(default)]
    pub mtime: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    /// Project items that read it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub items: Vec<u64>,
    /// A comp uses it (an unused item can't change a frame: reported, not required).
    #[serde(default)]
    pub used: bool,
    /// The file exists on the machine that wrote the manifest.
    #[serde(default)]
    pub exists: bool,
}

/// One text style's font, as the machine that wrote the manifest resolves it.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FontEntry {
    /// The requested font (family or PostScript name) and style.
    pub font: String,
    #[serde(default)]
    pub style: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub render_font: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub font_file: Option<FontFileLink>,
    /// `exact`, `substituted` (an explicit render font) or `missing` (a fallback face).
    pub status: String,
    /// The face drawn: PostScript name, family, origin (`bundled`, `system`, `linked`).
    #[serde(default)]
    pub postscript: String,
    #[serde(default)]
    pub family: String,
    #[serde(default)]
    pub origin: String,
    /// Its file (none for faces built into the program) and the sha256 of its bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(default)]
    pub size: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
}

// ---------------------------------------------------------------- files on disk

/// Size and whole-second mtime of a file (`None`: not there, or not a file).
pub fn stat(path: &str) -> Option<(u64, i64)> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let m = std::fs::metadata(path).ok().filter(|m| m.is_file())?;
        let mtime = m
            .modified()
            .ok()
            .map(|t| match t.duration_since(std::time::UNIX_EPOCH) {
                Ok(d) => i64::try_from(d.as_secs()).unwrap_or(i64::MAX),
                // Before 1970: negative seconds, rounded down like tar.
                Err(e) => {
                    let d = e.duration();
                    let s = i64::try_from(d.as_secs()).unwrap_or(i64::MAX);
                    if d.subsec_nanos() > 0 { s.saturating_neg().saturating_sub(1) } else { s.saturating_neg() }
                }
            })
            .unwrap_or(0);
        Some((m.len(), mtime))
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = path;
        None
    }
}

/// Remembered hashes: (size, mtime) → sha256 per path, so the 6 GB clip is read once.
fn hash_cache() -> &'static Mutex<Option<BTreeMap<String, (u64, i64, String)>>> {
    static C: Mutex<Option<BTreeMap<String, (u64, i64, String)>>> = Mutex::new(None);
    &C
}

#[cfg(not(target_arch = "wasm32"))]
fn hash_cache_file() -> Option<std::path::PathBuf> {
    if let Some(p) = std::env::var_os("EFFECTCRAFT_HASH_CACHE").filter(|p| !p.is_empty()) {
        return Some(p.into());
    }
    let home = std::env::var_os("HOME").filter(|h| !h.is_empty()).map(std::path::PathBuf::from);
    let base = if cfg!(target_os = "macos") {
        home?.join("Library/Caches/EffectCraft")
    } else if let Some(x) = std::env::var_os("XDG_CACHE_HOME").filter(|x| !x.is_empty()) {
        std::path::PathBuf::from(x).join("effectcraft")
    } else {
        home?.join(".cache/effectcraft")
    };
    Some(base.join("file-hashes.json"))
}

fn cached_hash(path: &str, size: u64, mtime: i64) -> Option<String> {
    let mut c = hash_cache().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    #[cfg(not(target_arch = "wasm32"))]
    if c.is_none() {
        let loaded = hash_cache_file().and_then(|f| std::fs::read(f).ok()).and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        *c = Some(loaded);
    }
    c.as_ref()?.get(path).filter(|(s, m, _)| *s == size && *m == mtime).map(|(_, _, h)| h.clone())
}

fn remember_hashes(found: Vec<(String, u64, i64, String)>) {
    if found.is_empty() {
        return;
    }
    let mut c = hash_cache().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let map = c.get_or_insert_with(BTreeMap::new);
    for (path, size, mtime, h) in found {
        map.insert(path, (size, mtime, h));
    }
    #[cfg(not(target_arch = "wasm32"))]
    if let (Some(file), Ok(bytes)) = (hash_cache_file(), serde_json::to_vec(map)) {
        // Best effort: a cache that can't be written is only slower next time.
        if let Some(dir) = file.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let tmp = file.with_extension(format!("tmp{}", std::process::id()));
        if std::fs::write(&tmp, bytes).is_ok() && std::fs::rename(&tmp, &file).is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
    }
}

/// The remembered sha256 of a file as it is now, without reading it.
pub fn cached_sha256(path: &str) -> Option<String> {
    let (size, mtime) = stat(path)?;
    cached_hash(path, size, mtime)
}

/// sha256 of a file, streamed (any size), and remembered by size + mtime. `progress` gets the
/// bytes read so far.
pub fn hash_file(path: &str, progress: &mut dyn FnMut(u64)) -> Option<String> {
    let (size, mtime) = stat(path)?;
    if let Some(h) = cached_hash(path, size, mtime) {
        return Some(h);
    }
    let h = hash_uncached(path, progress)?;
    // Changed while it was read: not remembered.
    if stat(path) == Some((size, mtime)) {
        remember_hashes(vec![(path.to_string(), size, mtime, h.clone())]);
    }
    Some(h)
}

#[cfg(not(target_arch = "wasm32"))]
fn hash_uncached(path: &str, progress: &mut dyn FnMut(u64)) -> Option<String> {
    use std::io::Read;
    let mut f = std::fs::File::open(path).ok()?;
    let mut h = effectcraft_segment::sha256::Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    let mut done = 0u64;
    loop {
        let n = match f.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return None,
        };
        h.update(buf.get(..n)?);
        done = done.saturating_add(n as u64);
        progress(done);
    }
    Some(h.finish_hex())
}

#[cfg(target_arch = "wasm32")]
fn hash_uncached(_: &str, _: &mut dyn FnMut(u64)) -> Option<String> {
    None
}

// ---------------------------------------------------------------- what a project needs

/// Items a comp layer reads (directly; precomps are comps, all of which count).
fn used_items(project: &Project) -> BTreeSet<ItemId> {
    // (A used item's proxy is read in its place: it counts as used with its item.)
    project.comps().flat_map(|(_, c)| c.layers.iter().filter_map(|l| l.source.item())).collect()
}

#[derive(Default)]
struct Need {
    kind: &'static str,
    items: BTreeSet<u64>,
    used: bool,
}

/// Every file path the project reads, with its kind, items and whether a comp uses it.
fn needed_files(project: &Project, sidecars: &mut dyn FnMut(&str) -> Vec<String>) -> BTreeMap<String, Need> {
    let used = used_items(project);
    let mut out: BTreeMap<String, Need> = BTreeMap::new();
    let mut add = |path: &str, kind: &'static str, item: Option<u64>, is_used: bool| {
        let path = path.trim();
        if path.is_empty() {
            return;
        }
        let n = out.entry(path.to_string()).or_insert_with(|| Need { kind, ..Default::default() });
        n.items.extend(item);
        n.used |= is_used;
    };
    for item in project.items.values() {
        let is_used = used.contains(&item.id);
        let main = match &item.kind {
            ItemKind::Footage(f) => Some((f, if f.sequence.is_empty() { "footage" } else { "sequence" })),
            _ => None,
        };
        for (f, kind) in main.into_iter().chain(item.proxy.as_ref().map(|p| (&p.footage, "proxy"))) {
            if f.kind == FootageKind::Data && f.data.is_some() {
                continue;
            }
            add(&f.path, kind, Some(item.id.0), is_used);
            for frame in &f.sequence {
                add(frame, kind, Some(item.id.0), is_used);
            }
            if f.kind == FootageKind::Model && !f.path.is_empty() {
                for side in sidecars(&f.path).into_iter().take(MAX_SIDECARS) {
                    add(&side, "model", Some(item.id.0), is_used);
                }
            }
        }
    }
    for w in crate::collect_report::dependency_warnings(project) {
        if w["kind"] == "external_effect_file"
            && let Some(p) = w["path"].as_str()
        {
            add(p, "effect", None, true);
        }
    }
    for id in applied_effects(project) {
        if let Some(src) = crate::effects::plugin::plugin(&id).map(|p| p.source())
            && src.to_ascii_lowercase().ends_with(".wasm")
        {
            add(&src, "plugin", None, true);
        }
    }
    out
}

/// Effect ids applied anywhere in the project.
fn applied_effects(project: &Project) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for (_, c) in project.comps() {
        for l in &c.layers {
            for g in l.effects().into_iter().flat_map(|fx| fx.groups()) {
                if let GroupKind::Effect { effect } = &g.kind {
                    out.insert(effect.clone());
                }
            }
        }
    }
    out
}

/// Files a 3D model reads beside itself: glTF buffers and images, OBJ material libraries and
/// their texture maps. Embedded (`data:`) resources need nothing.
pub fn model_sidecars(read: &dyn Fn(&str) -> Option<Vec<u8>>, path: &str) -> Vec<String> {
    let dir = std::path::Path::new(path).parent().map(|d| d.to_path_buf()).unwrap_or_default();
    let join = |rel: &str| -> Option<String> {
        let rel = percent_decode(rel.trim());
        if rel.is_empty() || rel.starts_with("data:") || rel.contains("://") {
            return None;
        }
        let p = std::path::Path::new(&rel);
        Some(if p.is_absolute() { rel.clone() } else { dir.join(p).to_string_lossy().into_owned() })
    };
    let lower = path.to_ascii_lowercase();
    let mut out = vec![];
    if lower.ends_with(".gltf") {
        let Some(doc) = read(path).and_then(|b| serde_json::from_slice::<Value>(&b).ok()) else { return out };
        for key in ["buffers", "images"] {
            for e in doc[key].as_array().into_iter().flatten() {
                out.extend(e["uri"].as_str().and_then(join));
            }
        }
    } else if lower.ends_with(".obj") {
        let Some(text) = read(path).map(|b| String::from_utf8_lossy(&b).into_owned()) else { return out };
        for line in text.lines() {
            if let Some(rest) = line.trim_start().strip_prefix("mtllib ") {
                out.extend(join(rest));
            }
        }
        let mtls = out.clone();
        for mtl in mtls {
            let Some(text) = read(&mtl).map(|b| String::from_utf8_lossy(&b).into_owned()) else { continue };
            let mdir = std::path::Path::new(&mtl).parent().map(|d| d.to_path_buf()).unwrap_or_default();
            for line in text.lines() {
                let line = line.trim_start();
                let key = line.split_whitespace().next().unwrap_or("").to_ascii_lowercase();
                if key.starts_with("map_") || matches!(key.as_str(), "bump" | "disp" | "decal" | "norm" | "refl") {
                    // Options come first (`-bm 1 file.png`): the file is the last token.
                    if let Some(file) = line.split_whitespace().last().filter(|f| !f.starts_with('-') && *f != key) {
                        let p = std::path::Path::new(file);
                        out.push(if p.is_absolute() { file.to_string() } else { mdir.join(p).to_string_lossy().into_owned() });
                    }
                }
            }
            if out.len() > MAX_SIDECARS {
                break;
            }
        }
    }
    out.sort();
    out.dedup();
    out.truncate(MAX_SIDECARS);
    out
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while let Some(&c) = b.get(i) {
        if c == b'%'
            && let Some(hex) = b.get(i + 1..i + 3).and_then(|h| std::str::from_utf8(h).ok())
            && let Ok(v) = u8::from_str_radix(hex, 16)
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(c);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Every distinct (font, style, render font, linked file) a text value asks for: static values,
/// keyframes and per-character runs ([`crate::collect_report::dependency_warnings`]).
fn font_requests(project: &Project) -> Vec<(String, String, Option<String>, Option<FontFileLink>)> {
    let mut seen = BTreeSet::new();
    let mut out = vec![];
    for w in crate::collect_report::dependency_warnings(project) {
        if w["kind"] != "font_required" {
            continue;
        }
        let font = w["font"].as_str().unwrap_or("").to_string();
        let style = w["style"].as_str().unwrap_or("").to_string();
        let render = w["render_font"].as_str().filter(|s| !s.is_empty()).map(str::to_string);
        let link: Option<FontFileLink> = serde_json::from_value(w["font_file"].clone()).ok();
        let key = (font.clone(), style.clone(), render.clone(), link.as_ref().map(|l| (l.path.clone(), l.sha256.clone())));
        if seen.insert(key) {
            out.push((font, style, render, link));
        }
    }
    out
}

/// Resolve one text style's font on this machine (as rendering does) and describe the face.
fn resolve_font(font: &str, style: &str, render: Option<&str>, link: Option<&FontFileLink>) -> FontEntry {
    use crate::text::fonts;
    if render.is_none()
        && let Some(l) = link
    {
        let _ = fonts::register_linked(font, l);
    }
    let (resolved, status) = fonts::resolve_for_render(font, style, render);
    let face = fonts::face(resolved.face);
    let file = match &face.info.data {
        fonts::FaceData::File(p) => Some(p.to_string_lossy().into_owned()),
        // A linked face is held in memory; its file is the link's.
        fonts::FaceData::Shared(_) if face.info.origin == "linked" => link.map(|l| l.path.clone()),
        _ => None,
    };
    let size = file.as_deref().and_then(stat).map_or(0, |(s, _)| s);
    FontEntry {
        font: font.to_string(),
        style: style.to_string(),
        render_font: render.map(str::to_string),
        font_file: link.cloned(),
        status: match status {
            fonts::FontStatus::Exact => "exact",
            fonts::FontStatus::Substituted => "substituted",
            fonts::FontStatus::Missing => "missing",
        }
        .into(),
        postscript: face.info.postscript.clone().unwrap_or_else(|| face.info.full_name.clone()),
        family: face.info.family.clone(),
        origin: face.info.origin.to_string(),
        file,
        size,
        sha256: face.content_hash().map(str::to_string),
    }
}

/// Expressions that read the clock or an unseeded random source.
fn nondeterministic(project: &Project) -> Vec<Value> {
    let mut out = vec![];
    for (cid, c) in project.comps() {
        for l in &c.layers {
            l.props.walk("", &mut |path, p| {
                let Some(e) = p.expr.as_ref().filter(|e| e.enabled) else { return };
                let uses: Vec<&str> = [("Math.random", "Math.random"), ("Date.now", "Date"), ("new Date", "Date"), ("Date(", "Date")]
                    .iter()
                    .filter(|(needle, _)| e.text.contains(needle))
                    .map(|(_, name)| *name)
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .collect();
                if !uses.is_empty() {
                    out.push(json!({"comp": cid.0, "layer": l.id.0, "property": path, "name": p.name, "uses": uses}));
                }
            });
        }
    }
    out
}

/// How much to hash.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Hash {
    None,
    /// Files up to [`SMALL_FILE`].
    Small,
    All,
}

/// The manifest of `project` on this machine.
pub fn manifest(project: &Project, read: &dyn Fn(&str) -> Option<Vec<u8>>, hash: Hash) -> Manifest {
    let needs = needed_files(project, &mut |p| model_sidecars(read, p));
    let mut files: Vec<FileEntry> = needs
        .into_iter()
        .map(|(path, n)| {
            let st = stat(&path);
            FileEntry {
                size: st.map_or(0, |s| s.0),
                mtime: st.map_or(0, |s| s.1),
                exists: st.is_some(),
                path,
                kind: n.kind.into(),
                sha256: None,
                items: n.items.into_iter().collect(),
                used: n.used,
            }
        })
        .collect();
    let mut fonts: Vec<FontEntry> = font_requests(project).iter().map(|(f, s, r, l)| resolve_font(f, s, r.as_deref(), l.as_ref())).collect();
    // A linked font must sit at its own path (the project names it); other font files go to the
    // server's font folder, so they are uploaded by the mirror from `fonts`.
    for l in fonts.iter().filter_map(|f| f.font_file.as_ref()) {
        if !files.iter().any(|e| e.path == l.path) {
            let st = stat(&l.path);
            files.push(FileEntry {
                path: l.path.clone(),
                kind: "font".into(),
                size: st.map_or(0, |s| s.0),
                mtime: st.map_or(0, |s| s.1),
                sha256: Some(l.sha256.to_ascii_lowercase()),
                items: vec![],
                used: true,
                exists: st.is_some(),
            });
        }
    }
    if hash != Hash::None {
        use rayon::prelude::*;
        files.par_iter_mut().filter(|e| e.exists && e.sha256.is_none() && (hash == Hash::All || e.size <= SMALL_FILE)).for_each(|e| {
            e.sha256 = hash_file(&e.path, &mut |_| {});
        });
    }
    fonts.sort_by(|a, b| (&a.font, &a.style).cmp(&(&b.font, &b.style)));
    let (cjk_locale, forms) = crate::text::fonts::cjk_locale();
    let totals = Totals {
        files: files.len(),
        bytes: files.iter().map(|f| f.size).fold(0u64, u64::saturating_add),
        fonts: fonts.len(),
        missing_here: files.iter().filter(|f| !f.exists).count(),
    };
    Manifest {
        version: 1,
        host: HostInfo { os: std::env::consts::OS.into(), cjk_locale, cjk_forms: forms.into() },
        files,
        fonts,
        effects: applied_effects(project).into_iter().collect(),
        nondeterministic: nondeterministic(project),
        totals,
    }
}

// ---------------------------------------------------------------- the check

/// One thing the server lacks.
#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Missing {
    /// The file's kind, `font`, `effect` or `locale`.
    pub kind: String,
    /// `missing`, `size`, `mtime`, `hash`, `different-font`, `missing-font`, `not-loaded`, `locale`.
    pub reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub font: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub style: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub items: Vec<u64>,
    pub detail: String,
}

impl Missing {
    fn file(e: &FileEntry, reason: &str, detail: String) -> Missing {
        Missing { kind: e.kind.clone(), reason: reason.into(), path: Some(e.path.clone()), font: None, style: None, items: e.items.clone(), detail }
    }
    fn label(&self) -> String {
        match (&self.font, &self.path) {
            (Some(f), _) => format!("font “{f}{}”", self.style.as_deref().filter(|s| !s.is_empty()).map(|s| format!(" {s}")).unwrap_or_default()),
            (None, Some(p)) => format!("{} “{}”", self.kind, p),
            _ => self.detail.clone(),
        }
    }
}

/// Check a manifest written on the Mac against this machine. `verify_hash`: hash every file
/// that has a sha256 (slow for big media), not only those whose mtime differs.
pub fn check(manifest: &Manifest, verify_hash: bool) -> (Vec<Missing>, Vec<Missing>) {
    use rayon::prelude::*;
    let results: Vec<(bool, Option<Missing>)> = manifest
        .files
        .par_iter()
        .map(|e| {
            // Missing on the Mac too: both machines show the same placeholder.
            if !e.exists {
                return (e.used, None);
            }
            let found = match stat(&e.path) {
                None => Some(Missing::file(e, "missing", "not on this machine".into())),
                Some((size, _)) if size != e.size => Some(Missing::file(e, "size", format!("{size} bytes here, {} on the Mac", e.size))),
                // Copied without its mtime: the content decides (hashed once, then remembered).
                Some((_, mtime)) if mtime != e.mtime || verify_hash => match &e.sha256 {
                    Some(want) => match hash_file(&e.path, &mut |_| {}) {
                        Some(h) if h.eq_ignore_ascii_case(want) => None,
                        Some(_) => Some(Missing::file(e, "hash", "same size, different content".into())),
                        None => Some(Missing::file(e, "missing", "cannot be read here".into())),
                    },
                    None if mtime != e.mtime => {
                        Some(Missing::file(e, "mtime", format!("same size, modified {mtime} here and {} on the Mac (no sha256 to compare: run the mirror)", e.mtime)))
                    }
                    None => None,
                },
                Some(_) => None,
            };
            (e.used, found)
        })
        .collect();
    let (mut missing, mut warnings) = (vec![], vec![]);
    for (used, m) in results {
        if let Some(m) = m {
            if used { missing.push(m) } else { warnings.push(m) }
        }
    }
    for want in &manifest.fonts {
        let here = resolve_font(&want.font, &want.style, want.render_font.as_deref(), want.font_file.as_ref());
        let same = match (&want.sha256, &here.sha256) {
            (Some(a), Some(b)) => a.eq_ignore_ascii_case(b),
            // No bytes to compare (a face the Mac couldn't read): the name decides.
            _ => want.postscript.eq_ignore_ascii_case(&here.postscript),
        };
        if !same {
            let reason = if here.status == "missing" && want.status != "missing" { "missing-font" } else { "different-font" };
            missing.push(Missing {
                kind: "font".into(),
                reason: reason.into(),
                path: want.file.clone(),
                font: Some(want.font.clone()),
                style: Some(want.style.clone()),
                items: vec![],
                detail: format!(
                    "the Mac draws {} ({}), this machine {} ({}{})",
                    want.postscript,
                    want.status,
                    here.postscript,
                    here.status,
                    here.file.as_deref().map(|f| format!(", {f}")).unwrap_or_default()
                ),
            });
        }
    }
    for id in &manifest.effects {
        if crate::effects::find(id).is_none() {
            missing.push(Missing {
                kind: "effect".into(),
                reason: "not-loaded".into(),
                path: None,
                font: None,
                style: None,
                items: vec![],
                detail: format!("effect `{id}` is not available here (a plug-in that isn't loaded?)"),
            });
        }
    }
    let (_, forms) = crate::text::fonts::cjk_locale();
    if !manifest.fonts.is_empty() && !manifest.host.cjk_forms.is_empty() && manifest.host.cjk_forms != forms {
        missing.push(Missing {
            kind: "locale".into(),
            reason: "locale".into(),
            path: None,
            font: None,
            style: None,
            items: vec![],
            detail: format!(
                "Han text would use {forms} glyph forms here, {} on the Mac: set EFFECTCRAFT_CJK_LOCALE={} on this machine",
                manifest.host.cjk_forms, manifest.host.cjk_locale
            ),
        });
    }
    (missing, warnings)
}

/// One line for a status bar: `2 missing: font “SF Pro Display Bold” · footage “…/clip.mov”`.
pub fn summary(missing: &[Missing]) -> String {
    if missing.is_empty() {
        return "nothing missing".into();
    }
    let shown: Vec<String> = missing.iter().take(3).map(Missing::label).collect();
    let more = missing.len().saturating_sub(shown.len());
    format!("{} missing: {}{}", missing.len(), shown.join(" · "), if more > 0 { format!(" · {more} more") } else { String::new() })
}

/// Font, style, render font, linked file (path, sha256).
type FontReq = (String, String, Option<String>, Option<(String, String)>);

/// What one comp asks for: the items its layers read, fonts, effect files and effects.
#[derive(Default)]
struct CompNeeds {
    sources: Vec<ItemId>,
    fonts: Vec<FontReq>,
    effect_files: Vec<String>,
    effects: BTreeSet<String>,
}

fn comp_needs(c: &effectcraft_project::Comp) -> CompNeeds {
    let mut n = CompNeeds::default();
    for l in &c.layers {
        n.sources.extend(l.source.item());
        l.props.walk("", &mut |_, p| {
            for w in crate::collect_report::property_warnings(p) {
                if w["kind"] == "font_required" {
                    let req: FontReq = (
                        w["font"].as_str().unwrap_or("").to_string(),
                        w["style"].as_str().unwrap_or("").to_string(),
                        w["render_font"].as_str().filter(|s| !s.is_empty()).map(str::to_string),
                        serde_json::from_value::<FontFileLink>(w["font_file"].clone()).ok().map(|l| (l.path, l.sha256)),
                    );
                    if !n.fonts.contains(&req) {
                        n.fonts.push(req);
                    }
                } else if let Some(path) = w["path"].as_str().filter(|_| w["kind"] == "external_effect_file") {
                    n.effect_files.push(path.to_string());
                }
            }
        });
        for g in l.effects().into_iter().flat_map(|fx| fx.groups()) {
            if let GroupKind::Effect { effect } = &g.kind {
                n.effects.insert(effect.clone());
            }
        }
    }
    n
}

/// [`blockers`]' memory between revisions: each comp's needs by its `Arc` (a sync patch replaces
/// only the comps it changes) and how each font resolved.
#[derive(Default)]
pub struct BlockerCache {
    comps: Vec<(std::sync::Arc<effectcraft_project::Comp>, CompNeeds)>,
    fonts: BTreeMap<FontReq, Option<String>>,
    rescanned: Option<web_time::Instant>,
}

/// What would render wrong on this machine for `project` itself: used footage that isn't there,
/// fonts that fall back and effects that aren't loaded. The frame server refuses frames while
/// this is non-empty (without a manifest it can't tell a different font version;
/// `cloud.preflight` can).
pub fn blockers(project: &Project) -> Vec<Missing> {
    blockers_cached(project, &mut BlockerCache::default())
}

/// [`blockers`], recomputing only comps that changed since the last call with `cache`.
pub fn blockers_cached(project: &Project, cache: &mut BlockerCache) -> Vec<Missing> {
    let mut comps = Vec::new();
    for item in project.items.values() {
        if let ItemKind::Comp(c) = &item.kind {
            let needs = match cache.comps.iter().position(|(old, _)| std::sync::Arc::ptr_eq(old, c)) {
                Some(i) => cache.comps.swap_remove(i).1,
                None => comp_needs(c),
            };
            comps.push((c.clone(), needs));
        }
    }
    cache.comps = comps;
    let used: BTreeSet<ItemId> = cache.comps.iter().flat_map(|(_, n)| n.sources.iter().copied()).collect();
    let mut out: Vec<Missing> = vec![];
    let missing = |kind: &str, path: &str, items: Vec<u64>, detail: &str| Missing {
        kind: kind.into(),
        reason: "missing".into(),
        path: Some(path.into()),
        font: None,
        style: None,
        items,
        detail: detail.into(),
    };
    for item in project.items.values().filter(|i| used.contains(&i.id)) {
        let main = match &item.kind {
            ItemKind::Footage(f) => Some((f, if f.sequence.is_empty() { "footage" } else { "sequence" })),
            _ => None,
        };
        for (f, kind) in main.into_iter().chain(item.proxy.as_ref().map(|p| (&p.footage, "proxy"))) {
            if (f.kind == FootageKind::Data && f.data.is_some()) || (f.path.is_empty() && f.sequence.is_empty()) {
                continue;
            }
            // A sequence is there while any of its frames is (footage.check's rule).
            let present = stat(&f.path).is_some() || (!f.sequence.is_empty() && f.sequence.iter().any(|p| stat(p).is_some()));
            if !present {
                out.push(missing(kind, &f.path, vec![item.id.0], "not on this machine"));
            }
        }
    }
    let files: BTreeSet<&String> = cache.comps.iter().flat_map(|(_, n)| n.effect_files.iter()).collect();
    for f in files {
        if stat(f).is_none() {
            out.push(missing("effect", f, vec![], "not on this machine"));
        }
    }
    let fonts: BTreeSet<FontReq> = cache.comps.iter().flat_map(|(_, n)| n.fonts.iter().cloned()).collect();
    for req in &fonts {
        let fallback = match cache.fonts.get(req) {
            Some(f) => f.clone(),
            None => {
                let link = req.3.as_ref().map(|(path, sha256)| FontFileLink { path: path.clone(), sha256: sha256.clone() });
                let here = resolve_font(&req.0, &req.1, req.2.as_deref(), link.as_ref());
                let f = (here.status == "missing").then_some(here.postscript);
                cache.fonts.insert(req.clone(), f.clone());
                f
            }
        };
        if let Some(fb) = fallback {
            out.push(Missing {
                kind: "font".into(),
                reason: "missing-font".into(),
                path: None,
                font: Some(req.0.clone()),
                style: Some(req.1.clone()),
                items: vec![],
                detail: format!("would be drawn with {fb} here"),
            });
        }
    }
    // A font the mirror just copied in is found by a rescan (at most every 30 s).
    if out.iter().any(|m| m.kind == "font") && cache.rescanned.is_none_or(|t| t.elapsed().as_secs() >= 30) {
        cache.rescanned = Some(web_time::Instant::now());
        crate::text::fonts::rescan_system();
        cache.fonts.retain(|_, missing| missing.is_none());
    }
    let effects: BTreeSet<&String> = cache.comps.iter().flat_map(|(_, n)| n.effects.iter()).collect();
    for id in effects {
        if crate::effects::find(id).is_none() {
            out.push(Missing {
                kind: "effect".into(),
                reason: "not-loaded".into(),
                path: None,
                font: None,
                style: None,
                items: vec![],
                detail: format!("effect `{id}` is not available here"),
            });
        }
    }
    out
}

// ---------------------------------------------------------------- uploads the cloud has confirmed

/// Files the mirror confirmed on the server: path → (size, mtime). Process-wide: one cloud per
/// app.
fn confirmed() -> &'static Mutex<(u64, BTreeMap<String, (u64, i64)>)> {
    static C: Mutex<(u64, BTreeMap<String, (u64, i64)>)> = Mutex::new((0, BTreeMap::new()));
    &C
}

struct PendingCache {
    key: (u64, u64),
    at: web_time::Instant,
    value: Value,
}

fn pending_cache() -> &'static Mutex<Option<PendingCache>> {
    static C: Mutex<Option<PendingCache>> = Mutex::new(None);
    &C
}

/// The files `project` uses that the server hasn't confirmed: (path, size, kind, items).
pub fn pending_uploads(project: &Project, read: &dyn Fn(&str) -> Option<Vec<u8>>) -> Value {
    let (_, have) = {
        let c = confirmed().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        (c.0, c.1.clone())
    };
    let needs = needed_files(project, &mut |p| model_sidecars(read, p));
    let mut pending = vec![];
    let mut missing_here = vec![];
    let mut bytes = 0u64;
    let mut font_files: Vec<(String, &'static str)> = vec![];
    for (path, _, _, link) in font_requests(project).iter().map(|(f, s, r, l)| (resolve_font(f, s, r.as_deref(), l.as_ref()), f, s, l)) {
        if path.origin != "bundled"
            && let Some(file) = path.file
        {
            font_files.push((file, if link.is_some() { "font" } else { "fontFile" }));
        }
    }
    let used = needs.into_iter().filter(|(_, n)| n.used).map(|(p, n)| (p, n.kind, n.items.into_iter().collect::<Vec<_>>()));
    for (path, kind, items) in used.chain(font_files.into_iter().map(|(p, k)| (p, k, vec![]))) {
        match stat(&path) {
            None => missing_here.push(json!({"path": path, "kind": kind})),
            Some((size, mtime)) if have.get(&path) != Some(&(size, mtime)) => {
                bytes = bytes.saturating_add(size);
                pending.push(json!({"path": path, "kind": kind, "size": size, "mtime": mtime, "items": items}));
            }
            Some(_) => {}
        }
    }
    pending.sort_by(|a, b| a["path"].as_str().cmp(&b["path"].as_str()));
    pending.dedup();
    json!({"count": pending.len(), "bytes": bytes, "pending": pending, "missingHere": missing_here, "confirmed": have.len()})
}

fn pending_cmd(s: &mut Session, _: &Value) -> Result<Value> {
    let generation = confirmed().lock().unwrap_or_else(std::sync::PoisonError::into_inner).0;
    let key = (s.revision, generation);
    {
        let c = pending_cache().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        // Disk changes (a file finished copying) show within two seconds.
        if let Some(c) = c.as_ref().filter(|c| c.key == key && c.at.elapsed().as_secs_f64() < 2.0) {
            return Ok(c.value.clone());
        }
    }
    let services = s.services.clone();
    let value = pending_uploads(&s.project, &|p| services.read_file(p).ok());
    *pending_cache().lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(PendingCache { key, at: web_time::Instant::now(), value: value.clone() });
    Ok(value)
}

fn mark_uploaded(_: &mut Session, p: &Value) -> Result<Value> {
    let c = "cloud.markUploaded";
    let mut guard = confirmed().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if p.get("clear").and_then(Value::as_bool).unwrap_or(false) {
        guard.1.clear();
    }
    let mut n = 0;
    for f in p.get("files").and_then(Value::as_array).into_iter().flatten() {
        let (Some(path), Some(size)) = (f["path"].as_str(), f["size"].as_u64()) else {
            return Err(bad(c, "files: [{path, size, mtime}]"));
        };
        guard.1.insert(path.to_string(), (size, f["mtime"].as_i64().unwrap_or(0)));
        n += 1;
    }
    guard.0 = guard.0.wrapping_add(1);
    Ok(json!({"marked": n, "confirmed": guard.1.len()}))
}

// ---------------------------------------------------------------- commands

fn hash_p(p: &Value) -> Result<Hash> {
    match str_p(p, "hash").unwrap_or("small") {
        "none" => Ok(Hash::None),
        "small" => Ok(Hash::Small),
        "all" => Ok(Hash::All),
        h => Err(bad("project.dependencies", format!("hash: none|small|all, not `{h}`"))),
    }
}

fn dependencies(s: &mut Session, p: &Value) -> Result<Value> {
    let hash = hash_p(p)?;
    let services = s.services.clone();
    let m = manifest(&s.project, &|path| services.read_file(path).ok(), hash);
    serde_json::to_value(m).map_err(|e| EngineError::Other(e.to_string()))
}

fn preflight(s: &mut Session, p: &Value) -> Result<Value> {
    let c = "cloud.preflight";
    let verify_hash = str_p(p, "verify") == Some("hash");
    let manifest: Option<Manifest> = match (p.get("manifest"), str_p(p, "manifestPath")) {
        (Some(m), _) => Some(serde_json::from_value(m.clone()).map_err(|e| bad(c, format!("manifest: {e}")))?),
        (None, Some(path)) => {
            let bytes = s.services.read_file(path).map_err(|e| bad(c, format!("cannot read {path}: {e}")))?;
            Some(serde_json::from_slice(&bytes).map_err(|e| bad(c, format!("{path}: {e}")))?)
        }
        (None, None) => None,
    };
    let (missing, warnings, nondet, counts) = match manifest {
        Some(m) => {
            let (missing, warnings) = check(&m, verify_hash);
            let counts = json!({"files": m.files.len(), "fonts": m.fonts.len(), "effects": m.effects.len(), "missing": missing.len()});
            (missing, warnings, m.nondeterministic, counts)
        }
        None => {
            // This machine's own project: look for its files first (footage.check marks them).
            if s.project.items.values().any(|i| matches!(i.kind, ItemKind::Footage(_))) {
                crate::footage_check::start(s, None, true)?;
            }
            let missing = blockers(&s.project);
            let counts = json!({"files": needed_files(&s.project, &mut |_| vec![]).len(), "fonts": font_requests(&s.project).len(), "missing": missing.len()});
            (missing, vec![], nondeterministic(&s.project), counts)
        }
    };
    Ok(json!({
        "ok": missing.is_empty(),
        "summary": summary(&missing),
        "missing": missing,
        "warnings": warnings,
        "counts": counts,
        "nondeterministic": nondet,
    }))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        query!(
            "project.dependencies",
            "Project Dependencies",
            "{hash?: none|small|all (default small: files up to 64 MB)} → {version, host: {os, cjkLocale, cjkForms}, files: [{path, kind: footage|sequence|proxy|effect|model|plugin|font, size, mtime, sha256?, items, used, exists}], fonts: [{font, style, renderFont?, fontFile?, status, postscript, family, origin, file?, size, sha256?}], effects, nondeterministic: [{comp, layer, property, uses}], totals} — every file and font the project reads, for mirroring it to a cloud render machine",
            dependencies
        ),
        query!(
            "cloud.preflight",
            "Cloud Preflight",
            "{manifest?: project.dependencies of the Mac, manifestPath?: file with it, verify?: size|hash} → {ok, summary, missing: [{kind, reason: missing|size|mtime|hash|missing-font|different-font|not-loaded|locale, path?, font?, style?, items, detail}], warnings (unused items), counts, nondeterministic} — on the render machine: whether it has everything to render frames exactly like the Mac; without a manifest it checks its own project (footage.check first). Cloud preview is refused while `ok` is false",
            preflight
        ),
        query!(
            "cloud.pendingUploads",
            "Cloud Pending Uploads",
            "{} → {count, bytes, pending: [{path, kind, size, mtime, items}], missingHere, confirmed} — files the project uses that the mirror hasn't confirmed on the cloud machine yet (frames render locally while count > 0)",
            pending_cmd
        ),
        query!(
            "cloud.markUploaded",
            "Cloud Mark Uploaded",
            "{files: [{path, size, mtime}], clear?: bool} → {marked, confirmed} — the mirror confirms files are on the cloud machine",
            mark_uploaded
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use effectcraft_keyframe::{CharStyle, StyleRun, TextDoc, Value as KValue};

    fn tmp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("ec-cloud-assets-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let _ = std::fs::create_dir_all(&d);
        d
    }

    fn text_session(font: &str) -> Session {
        let mut s = Session::default();
        s.execute("comp.new", json!({"name": "C", "width": 320, "height": 180})).ok();
        let doc = TextDoc {
            text: "ab".into(),
            font: font.into(),
            runs: vec![StyleRun { len: 1, style: CharStyle { font: "Inter".into(), ..Default::default() } }],
            ..Default::default()
        };
        s.execute("layer.newText", json!({"text": "ab"})).ok();
        let cid = s.active_comp_id();
        if let Some(c) = cid.and_then(|c| std::sync::Arc::make_mut(&mut s.project).comp_mut(c))
            && let Some(l) = c.layers.iter_mut().next()
            && let Some(p) = l.props.prop_mut("text/sourceText")
        {
            p.value = KValue::Text(Box::new(doc));
        }
        s
    }

    #[test]
    fn manifest_lists_footage_frames_proxies_and_effect_files_with_hashes() {
        let d = tmp("manifest");
        let clip = d.join("clip one.mov");
        std::fs::write(&clip, b"movie").ok();
        let frame = d.join("f_0001.png");
        std::fs::write(&frame, b"png").ok();
        let mut s = Session::default();
        let clip_s = clip.to_string_lossy().to_string();
        let frame_s = frame.to_string_lossy().to_string();
        let p = std::sync::Arc::make_mut(&mut s.project);
        let a = p.add_item("clip", Default::default(), None, ItemKind::Footage(effectcraft_project::Footage { path: clip_s.clone(), ..Default::default() }));
        p.add_item(
            "seq",
            Default::default(),
            None,
            ItemKind::Footage(effectcraft_project::Footage { path: frame_s.clone(), sequence: vec![frame_s.clone(), "/nowhere/f_0002.png".into()], ..Default::default() }),
        );
        let r = s.execute("project.dependencies", json!({})).unwrap();
        let m: Manifest = serde_json::from_value(r).unwrap();
        let clip_e = m.files.iter().find(|f| f.path == clip_s).unwrap();
        assert_eq!((clip_e.kind.as_str(), clip_e.size, clip_e.exists), ("footage", 5, true));
        assert_eq!(clip_e.items, vec![a.0]);
        assert_eq!(clip_e.sha256.as_deref(), Some(effectcraft_segment::sha256::hex(b"movie").as_str()));
        assert!(!clip_e.used, "no comp uses it yet");
        let gone = m.files.iter().find(|f| f.path == "/nowhere/f_0002.png").unwrap();
        assert_eq!((gone.kind.as_str(), gone.exists), ("sequence", false));
        assert_eq!(m.totals.missing_here, 1);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn preflight_reports_a_missing_file_changed_size_and_passes_a_present_one() {
        let d = tmp("preflight");
        let here = d.join("here.mov");
        std::fs::write(&here, b"12345").ok();
        let (size, mtime) = stat(&here.to_string_lossy()).unwrap();
        let entry = |path: &str, size: u64| FileEntry { path: path.into(), kind: "footage".into(), size, mtime, used: true, exists: true, ..Default::default() };
        let m = Manifest {
            version: 1,
            files: vec![entry(&here.to_string_lossy(), size), entry("/nowhere/clip.mov", 9), entry(&here.to_string_lossy(), 99)],
            ..Default::default()
        };
        let (missing, warnings) = check(&m, false);
        assert!(warnings.is_empty());
        let reasons: Vec<&str> = missing.iter().map(|m| m.reason.as_str()).collect();
        assert_eq!(reasons, vec!["missing", "size"]);
        assert!(summary(&missing).starts_with("2 missing: footage “/nowhere/clip.mov”"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_same_size_file_with_other_content_is_caught_by_its_hash() {
        let d = tmp("hash");
        let f = d.join("lut.cube");
        std::fs::write(&f, b"abc").ok();
        let path = f.to_string_lossy().to_string();
        let (size, mtime) = stat(&path).unwrap();
        let m = Manifest {
            files: vec![FileEntry {
                path,
                kind: "effect".into(),
                size,
                mtime: mtime - 10,
                sha256: Some(effectcraft_segment::sha256::hex(b"xyz")),
                used: true,
                exists: true,
                ..Default::default()
            }],
            ..Default::default()
        };
        let (missing, _) = check(&m, false);
        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0].reason, "hash");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn preflight_refuses_a_font_this_machine_does_not_have() {
        // The Mac drew this text with a font the server lacks: the server would silently use
        // its fallback face, so the preflight must refuse.
        let mut s = text_session("Inter");
        let r = s.execute("project.dependencies", json!({"hash": "none"})).unwrap();
        let mut m: Manifest = serde_json::from_value(r).unwrap();
        assert!(!m.fonts.is_empty());
        let ok = s.execute("cloud.preflight", json!({"manifest": m.clone()})).unwrap();
        assert_eq!(ok["ok"], true, "{ok}");
        // As if written on a Mac that has the font.
        let f = m.fonts.first_mut().unwrap();
        f.font = "EffectCraft Test Font That Nobody Has".into();
        f.postscript = "ECTestNobody-Bold".into();
        f.status = "exact".into();
        f.sha256 = Some("0".repeat(64));
        let r = s.execute("cloud.preflight", json!({"manifest": m})).unwrap();
        assert_eq!(r["ok"], false);
        assert_eq!(r["missing"][0]["kind"], "font");
        assert_eq!(r["missing"][0]["reason"], "missing-font");
        assert!(r["summary"].as_str().unwrap().contains("EffectCraft Test Font That Nobody Has"), "{r}");
    }

    #[test]
    fn blockers_name_a_missing_font_and_missing_used_footage() {
        let mut s = text_session("EffectCraft Test Font That Nobody Has");
        let b = blockers(&s.project);
        assert!(b.iter().any(|m| m.kind == "font" && m.font.as_deref() == Some("EffectCraft Test Font That Nobody Has")), "{b:?}");
        assert!(b.iter().all(|m| m.font.as_deref() != Some("Inter")));
        let r = s.execute("cloud.preflight", json!({})).unwrap();
        assert_eq!(r["ok"], false);
    }

    #[test]
    fn pending_uploads_shrink_as_the_mirror_confirms_files() {
        let d = tmp("pending");
        let f = d.join("new import.mov");
        std::fs::write(&f, b"new").ok();
        let path = f.to_string_lossy().to_string();
        let mut s = Session::default();
        s.execute("comp.new", json!({"name": "C"})).ok();
        let id = std::sync::Arc::make_mut(&mut s.project).add_item("new", Default::default(), None, ItemKind::Footage(effectcraft_project::Footage { path: path.clone(), ..Default::default() }));
        let p = pending_uploads(&s.project, &|_| None);
        assert!(p["pending"].as_array().unwrap().iter().all(|e| e["path"] != path.as_str()), "unused: not needed yet");
        let cid = s.active_comp_id().unwrap();
        s.execute("layer.addItem", json!({"comp": cid.0, "item": id.0, "time": 0.0})).unwrap();
        let p = pending_uploads(&s.project, &|_| None);
        assert!(p["pending"].as_array().unwrap().iter().any(|e| e["path"] == path.as_str()), "{p}");
        let (size, mtime) = stat(&path).unwrap();
        s.execute("cloud.markUploaded", json!({"files": [{"path": path, "size": size, "mtime": mtime}]})).unwrap();
        let p = pending_uploads(&s.project, &|_| None);
        assert!(p["pending"].as_array().unwrap().iter().all(|e| e["path"] != path.as_str()), "{p}");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// `CR7_PROJECT=/path/to/real.ecproj cargo test -p effectcraft-engine --lib guard_cost -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn guard_cost_on_a_real_project() {
        let Ok(path) = std::env::var("CR7_PROJECT") else { return };
        let mut s = Session::default();
        s.execute("file.open", json!({"path": path})).unwrap();
        let mut cache = BlockerCache::default();
        for label in ["first", "cached", "cached"] {
            let t = std::time::Instant::now();
            let n = blockers_cached(&s.project, &mut cache).len();
            eprintln!("{label}: {n} blockers in {:.1} ms", t.elapsed().as_secs_f64() * 1e3);
        }
        // One comp changed (as a sync patch does): only it is walked again.
        let cid = s.project.comps().next().map(|(id, _)| *id).unwrap();
        // The cache holds the old Arc, so this clones the comp (a new pointer).
        assert!(std::sync::Arc::make_mut(&mut s.project).comp_mut(cid).is_some());
        let t = std::time::Instant::now();
        blockers_cached(&s.project, &mut cache);
        eprintln!("one comp changed: {:.1} ms", t.elapsed().as_secs_f64() * 1e3);
    }

    #[test]
    fn gltf_and_obj_sidecars_are_found() {
        let files: BTreeMap<&str, &str> = [
            ("/m/scene.gltf", r#"{"buffers":[{"uri":"scene%20data.bin"},{"uri":"data:application/octet-stream;base64,AAAA"}],"images":[{"uri":"tex/a.png"}]}"#),
            ("/m/thing.obj", "mtllib thing.mtl\nv 0 0 0\n"),
            ("/m/thing.mtl", "newmtl a\nmap_Kd -bm 1 wood.jpg\nbump n.png\n"),
        ]
        .into_iter()
        .collect();
        let read = |p: &str| files.get(p).map(|s| s.as_bytes().to_vec());
        assert_eq!(model_sidecars(&read, "/m/scene.gltf"), vec!["/m/scene data.bin".to_string(), "/m/tex/a.png".to_string()]);
        assert_eq!(model_sidecars(&read, "/m/thing.obj"), vec!["/m/n.png".to_string(), "/m/thing.mtl".to_string(), "/m/wood.jpg".to_string()]);
    }

    #[test]
    fn nondeterministic_expressions_are_flagged() {
        let mut s = text_session("Inter");
        let cid = s.active_comp_id().unwrap();
        if let Some(c) = std::sync::Arc::make_mut(&mut s.project).comp_mut(cid)
            && let Some(l) = c.layers.iter_mut().next()
            && let Some(p) = l.props.prop_mut("transform/opacity")
        {
            p.expr = Some(effectcraft_project::Expression { text: "Math.random() * 100 + new Date().getSeconds()".into(), enabled: true });
        }
        let n = nondeterministic(&s.project);
        assert_eq!(n.len(), 1);
        assert_eq!(n[0]["uses"], json!(["Date", "Math.random"]));
    }
}
