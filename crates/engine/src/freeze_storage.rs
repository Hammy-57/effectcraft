//! Owned Smart Freeze storage. Retirement is reversible and never follows links or touches
//! source media. Call on a worker; no filesystem walks belong in the panel draw path.

use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use effectcraft_project::freeze::Version;
use serde::{Deserialize, Serialize};

pub const TEN_HOURS: u64 = 10 * 60 * 60;
const MAX_LEDGER: u64 = 16 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Entry {
    pub section: u64,
    pub version: Version,
    pub superseded_at: Option<u64>,
    pub trash_path: Option<String>,
}

#[derive(Default, Serialize, Deserialize)]
struct Ledger {
    entries: Vec<Entry>,
}

#[derive(Default, Serialize)]
pub struct Report {
    pub bytes: u64,
    pub retired: Vec<String>,
    pub errors: Vec<String>,
}

pub fn protected(project: &effectcraft_project::Project) -> BTreeSet<String> {
    let mut paths = BTreeSet::new();
    for section in &project.freeze.sections {
        if let Some(current) = &section.cache {
            paths.insert(current.directory.clone());
            if current.draft
                && let Some(full) = section.previous.iter().rev().find(|v| !v.draft)
            {
                paths.insert(full.directory.clone());
            }
        }
    }
    paths
}

struct Lock(PathBuf);
impl Drop for Lock {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir(&self.0);
    }
}

fn lock(root: &Path) -> Result<Lock, String> {
    let path = root.join(".ledger-lock");
    std::fs::create_dir(&path).map_err(|e| format!("Smart Freeze storage is busy or cannot be locked ({}): {e}", path.display()))?;
    Ok(Lock(path))
}

pub fn root(namespace: &str) -> Result<PathBuf, String> {
    if namespace.is_empty() || namespace.len() > 96 || !namespace.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-') {
        return Err("Smart Freeze storage has no valid project identity".into());
    }
    Ok(effectcraft_render::disk_cache::default_folder().join("smart-freeze").join(namespace))
}

fn load(root: &Path) -> Result<Ledger, String> {
    let path = root.join("versions.json");
    if !path.exists() {
        return Ok(Ledger::default());
    }
    if std::fs::symlink_metadata(&path).map_err(|e| e.to_string())?.file_type().is_symlink() {
        return Err("Smart Freeze version ledger is a link; left untouched".into());
    }
    let mut bytes = vec![];
    std::fs::File::open(path).map_err(|e| e.to_string())?.take(MAX_LEDGER + 1).read_to_end(&mut bytes).map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_LEDGER {
        return Err("Smart Freeze version ledger exceeds its size budget".into());
    }
    let ledger: Ledger = serde_json::from_slice(&bytes).map_err(|e| format!("Smart Freeze version ledger: {e}"))?;
    if ledger.entries.len() > 8192 {
        return Err("Smart Freeze version ledger exceeds its entry budget".into());
    }
    Ok(ledger)
}

fn save(root: &Path, ledger: &Ledger) -> Result<(), String> {
    let bytes = serde_json::to_vec(ledger).map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_LEDGER {
        return Err("Smart Freeze version ledger exceeds its size budget".into());
    }
    let temp = root.join(format!("versions-{}.tmp", crate::freeze_runtime::now_millis()));
    let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(&temp).map_err(|e| e.to_string())?;
    file.write_all(&bytes).and_then(|_| file.sync_all()).map_err(|e| e.to_string())?;
    std::fs::rename(&temp, root.join("versions.json")).map_err(|e| e.to_string())
}

fn owned(root: &Path, directory: &Path) -> Result<u64, String> {
    let canonical = root.canonicalize().map_err(|e| e.to_string())?;
    if directory.parent() != Some(root)
        || directory.canonicalize().map_err(|e| e.to_string())?.parent() != Some(canonical.as_path())
        || std::fs::symlink_metadata(directory).map_err(|e| e.to_string())?.file_type().is_symlink()
    {
        return Err("Smart Freeze refuses an external or linked cache folder".into());
    }
    let name = directory.file_name().and_then(|s| s.to_str()).ok_or("invalid cache folder name")?;
    if name.len() > 96 || !name.bytes().all(|b| b.is_ascii_digit() || b == b'-') {
        return Err("unrecognized Smart Freeze cache folder".into());
    }
    let marker = directory.join("owner.json");
    let meta = std::fs::symlink_metadata(&marker).map_err(|e| e.to_string())?;
    if !meta.is_file() || meta.len() > 1024 {
        return Err("Smart Freeze cache owner marker is invalid".into());
    }
    let owner: serde_json::Value = serde_json::from_slice(&std::fs::read(&marker).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    if owner["owner"] != "effectcraft-smart-freeze" || owner["version"] != 1 {
        return Err("unowned cache folder; left untouched".into());
    }
    let mut bytes = 0u64;
    let mut count = 0usize;
    for entry in std::fs::read_dir(directory).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        count = count.saturating_add(1);
        if count > effectcraft_project::freeze::MAX_FRAMES as usize + 8 {
            return Err("Smart Freeze cache has too many files".into());
        }
        let ty = entry.file_type().map_err(|e| e.to_string())?;
        let name = entry.file_name();
        let name = name.to_str().ok_or("unrecognized cache file name")?;
        let frame = name.strip_suffix(".ecfreeze").is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
        if !ty.is_file() || !(frame || matches!(name, "owner.json" | "version.json" | "recovery.json" | "job.json" | "terminal.json")) {
            return Err(format!("Smart Freeze protects unexpected cache content: {name}"));
        }
        bytes = bytes.saturating_add(entry.metadata().map_err(|e| e.to_string())?.len());
    }
    Ok(bytes)
}

/// Adopt earlier attached outputs, including drafts, without restarting existing grace periods.
pub fn adopt_previous(root: &Path, section: u64, versions: &[Version], now: u64) -> Result<(), String> {
    let _lock = lock(root)?;
    let mut ledger = load(root)?;
    for version in versions {
        if ledger.entries.iter().any(|entry| entry.version.directory == version.directory) {
            continue;
        }
        // An older cache may already have been removed outside the app. Retain the rest.
        if !Path::new(&version.directory).exists() {
            continue;
        }
        owned(root, Path::new(&version.directory))?;
        if ledger.entries.len() >= 8192 {
            return Err("Smart Freeze version ledger is full; earlier caches retained".into());
        }
        ledger.entries.push(Entry { section, version: version.clone(), superseded_at: Some(now), trash_path: None });
    }
    save(root, &ledger)
}

pub fn record(root: &Path, section: u64, version: &Version, now: u64) -> Result<(), String> {
    owned(root, Path::new(&version.directory))?;
    let _lock = lock(root)?;
    let mut ledger = load(root)?;
    for entry in &mut ledger.entries {
        if entry.section == section && entry.version.directory != version.directory && entry.superseded_at.is_none() {
            entry.superseded_at = Some(now);
        }
    }
    if let Some(entry) = ledger.entries.iter_mut().find(|e| e.version.directory == version.directory) {
        entry.superseded_at = None;
        entry.trash_path = None;
    } else {
        if ledger.entries.len() >= 8192 {
            return Err("Smart Freeze version ledger is full; cache remains attached".into());
        }
        ledger.entries.push(Entry { section, version: version.clone(), superseded_at: None, trash_path: None });
    }
    save(root, &ledger)
}

pub fn usage(root: &Path) -> Result<u64, String> {
    if !root.exists() {
        return Ok(0);
    }
    let mut pending = vec![(root.to_path_buf(), 0usize)];
    let (mut bytes, mut visited) = (0u64, 0usize);
    while let Some((path, depth)) = pending.pop() {
        visited = visited.saturating_add(1);
        if depth > 32 || visited > 1_000_000 {
            return Err("Smart Freeze storage scan exceeds its work budget".into());
        }
        let meta = std::fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
        if meta.file_type().is_symlink() {
            continue;
        }
        if meta.is_file() {
            bytes = bytes.saturating_add(meta.len());
        } else if meta.is_dir() {
            for entry in std::fs::read_dir(&path).map_err(|e| e.to_string())? {
                if pending.len() > 1_000_000 {
                    return Err("Smart Freeze storage scan exceeds its work budget".into());
                }
                pending.push((entry.map_err(|e| e.to_string())?.path(), depth.saturating_add(1)));
            }
        }
    }
    Ok(bytes)
}

pub fn retire(
    root: &Path,
    protected: &BTreeSet<String>,
    now: u64,
    immediate: bool,
    mover: impl Fn(&Path) -> Result<PathBuf, String>,
) -> Result<Report, String> {
    if !root.exists() {
        return Ok(Report::default());
    }
    let _lock = lock(root)?;
    let mut ledger = load(root)?;
    let mut report = Report::default();
    for index in 0..ledger.entries.len() {
        let Some(entry) = ledger.entries.get(index) else { continue };
        let Some(superseded) = entry.superseded_at else { continue };
        if entry.trash_path.is_some() || protected.contains(&entry.version.directory) || (!immediate && now.saturating_sub(superseded) < TEN_HOURS) {
            continue;
        }
        let path = Path::new(&entry.version.directory);
        let outcome = (|| {
            owned(root, path)?;
            // Recovery instructions travel with the directory even if the process stops
            // between the OS move and the ledger update.
            let recovery = serde_json::to_vec(&serde_json::json!({"original":path, "retiredUnix":now})).map_err(|e| e.to_string())?;
            let recovery_path = path.join("recovery.json");
            if !recovery_path.exists() {
                let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(&recovery_path).map_err(|e| e.to_string())?;
                file.write_all(&recovery).and_then(|_| file.sync_all()).map_err(|e| e.to_string())?;
            } else {
                let meta = std::fs::symlink_metadata(&recovery_path).map_err(|e| e.to_string())?;
                if !meta.is_file() || meta.len() > 4096 {
                    return Err("invalid recovery metadata; cache retained".into());
                }
                let mut bytes = vec![];
                std::fs::File::open(&recovery_path).map_err(|e| e.to_string())?.take(4097).read_to_end(&mut bytes).map_err(|e| e.to_string())?;
                if bytes.len() > 4096 {
                    return Err("recovery metadata exceeds its budget".into());
                }
                let data: serde_json::Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
                if data["original"] != path.to_string_lossy().as_ref() {
                    return Err("recovery metadata names another output; cache retained".into());
                }
            }
            mover(path)
        })();
        match outcome {
            Ok(destination) => {
                if let Some(entry) = ledger.entries.get_mut(index) {
                    entry.trash_path = Some(destination.to_string_lossy().into_owned());
                    report.retired.push(entry.version.directory.clone());
                }
                save(root, &ledger)?;
            }
            Err(e) => {
                if e == "Smart Freeze cancelled" {
                    return Err(e);
                }
                report.errors.push(e);
            }
        }
    }
    report.bytes = usage(root)?;
    Ok(report)
}

/// Native macOS Trash preserves a directory intact and gives us an exact recovery path.
#[cfg(target_os = "macos")]
pub fn move_to_trash(source: &Path) -> Result<PathBuf, String> {
    let home = std::env::var_os("HOME").ok_or("cannot locate the user's Trash")?;
    let trash = PathBuf::from(home).join(".Trash");
    std::fs::create_dir_all(&trash).map_err(|e| e.to_string())?;
    if std::fs::symlink_metadata(&trash).map_err(|e| e.to_string())?.file_type().is_symlink() {
        return Err("the Trash folder is a link; no files moved".into());
    }
    let name = source.file_name().and_then(|v| v.to_str()).ok_or("invalid cache folder name")?;
    let target = trash.join(format!("EffectCraft-{name}-{}", crate::freeze_runtime::now_millis()));
    std::fs::create_dir(&target).map_err(|e| e.to_string())?;
    if let Err(e) = std::fs::rename(source, &target) {
        let _ = std::fs::remove_dir(&target);
        return Err(format!("cache could not be moved to Trash: {e}"));
    }
    Ok(target)
}

#[cfg(target_os = "linux")]
pub fn move_to_trash(source: &Path) -> Result<PathBuf, String> {
    crate::freeze_trash::linux_move(source)
}

#[cfg(target_os = "windows")]
pub fn move_to_trash(source: &Path) -> Result<PathBuf, String> {
    crate::freeze_trash::windows_move(source)
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
pub fn move_to_trash(_: &Path) -> Result<PathBuf, String> {
    Err("Smart Freeze Trash retirement is not implemented on this platform; cache files were retained".into())
}

/// Only terminal, never-attached jobs qualify for abandoned-job cleanup. A crashed process
/// without this marker leaves its output protected rather than guessing that it is unused.
pub struct JobFolder(pub PathBuf);

impl Drop for JobFolder {
    fn drop(&mut self) {
        let path = self.0.join("terminal.json");
        if let Ok(mut file) = std::fs::OpenOptions::new().write(true).create_new(true).open(path) {
            let data = format!("{{\"finishedUnix\":{}}}", crate::freeze_runtime::now_millis() / 1000);
            let _ = file.write_all(data.as_bytes()).and_then(|_| file.sync_all());
        }
    }
}

pub fn maintain(
    root: &Path,
    protected: &BTreeSet<String>,
    now: u64,
    immediate: bool,
    budget: u64,
    retire_old: bool,
    mover: impl Fn(&Path) -> Result<PathBuf, String>,
) -> Result<Report, String> {
    let mut report =
        if retire_old || immediate { retire(root, protected, now, immediate, &mover)? } else { Report { bytes: usage(root)?, ..Default::default() } };
    if report.bytes <= budget || !root.exists() {
        return Ok(report);
    }
    let _lock = lock(root)?;
    let ledger = load(root)?;
    let mut candidates = vec![];
    for entry in std::fs::read_dir(root).map_err(|e| e.to_string())?.take(16384) {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        let name = path.to_string_lossy();
        if protected.contains(name.as_ref())
            || ledger.entries.iter().any(|e| e.version.directory == name)
            // Completed outputs might have attached before ledger maintenance failed.
            // Never infer "never attached" from a missing ledger entry alone.
            || path.join("version.json").exists()
            || !entry.file_type().map_err(|e| e.to_string())?.is_dir()
        {
            continue;
        }
        let marker = path.join("terminal.json");
        let Ok(meta) = std::fs::symlink_metadata(&marker) else { continue };
        if !meta.is_file() || meta.len() > 1024 {
            continue;
        }
        let data: serde_json::Value = match std::fs::read(&marker).ok().and_then(|b| serde_json::from_slice(&b).ok()) {
            Some(v) => v,
            None => continue,
        };
        if let Some(finished) = data["finishedUnix"].as_u64().filter(|t| now.saturating_sub(*t) >= 24 * 60 * 60) {
            candidates.push((finished, path));
        }
    }
    candidates.sort_by_key(|(finished, _)| *finished);
    for (_, path) in candidates {
        if report.bytes <= budget {
            break;
        }
        match owned(root, &path).and_then(|size| mover(&path).map(|_| size)) {
            Ok(size) => {
                report.bytes = report.bytes.saturating_sub(size);
                report.retired.push(path.to_string_lossy().into_owned());
            }
            Err(e) => report.errors.push(e),
        }
    }
    Ok(report)
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
pub fn restore(root: &Path, directory: &str) -> Result<(), String> {
    let _lock = lock(root)?;
    let mut ledger = load(root)?;
    let entry = ledger.entries.iter_mut().find(|e| e.version.directory == directory).ok_or("version is not in this project's recovery ledger")?;
    let source = PathBuf::from(entry.trash_path.as_ref().ok_or("version has not been retired")?);
    #[cfg(target_os = "macos")]
    let trash = PathBuf::from(std::env::var_os("HOME").ok_or("cannot locate Trash")?).join(".Trash");
    #[cfg(target_os = "linux")]
    let trash = crate::freeze_trash::linux_files()?;
    if source.parent() != Some(trash.as_path())
        || source.canonicalize().map_err(|e| e.to_string())?.parent() != Some(trash.canonicalize().map_err(|e| e.to_string())?.as_path())
        || std::fs::symlink_metadata(&source).map_err(|e| e.to_string())?.file_type().is_symlink()
        || !source.file_name().is_some_and(|n| n.to_string_lossy().starts_with("EffectCraft-"))
        || Path::new(directory).parent() != Some(root)
    {
        return Err("recovery path is outside the owned cache/Trash locations".into());
    }
    let recovery = source.join("recovery.json");
    let meta = std::fs::symlink_metadata(&recovery).map_err(|e| e.to_string())?;
    if !meta.is_file() || meta.len() > 4096 {
        return Err("recovery metadata is invalid".into());
    }
    let data: serde_json::Value = serde_json::from_slice(&std::fs::read(recovery).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    if data["original"] != directory {
        return Err("recovery metadata does not match the original cache location".into());
    }
    std::fs::create_dir(directory).map_err(|e| format!("original location is occupied or cannot be restored: {e}"))?;
    if let Err(e) = std::fs::rename(&source, directory) {
        let _ = std::fs::remove_dir(directory);
        return Err(e.to_string());
    }
    entry.trash_path = None;
    entry.superseded_at = Some(crate::freeze_runtime::now_millis() / 1000);
    save(root, &ledger)?;
    #[cfg(target_os = "linux")]
    if let Err(error) = crate::freeze_trash::linux_remove_info(&source) {
        log::warn!("cache restored, but Trash metadata cleanup failed: {error}");
    }
    Ok(())
}

#[cfg(target_os = "windows")]
pub fn restore(root: &Path, directory: &str) -> Result<(), String> {
    let _lock = lock(root)?;
    let mut ledger = load(root)?;
    let entry = ledger.entries.iter_mut().find(|e| e.version.directory == directory).ok_or("version is not in this project's recovery ledger")?;
    crate::freeze_trash::windows_restore(root, directory, entry.trash_path.as_deref().ok_or("version has not been retired")?)?;
    entry.trash_path = None;
    entry.superseded_at = Some(crate::freeze_runtime::now_millis() / 1000);
    save(root, &ledger)
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
pub fn restore(_: &Path, _: &str) -> Result<(), String> {
    Err("Smart Freeze Trash recovery is not implemented on this platform".into())
}

pub fn history(root: &Path) -> Result<Vec<Entry>, String> {
    Ok(load(root)?.entries)
}
