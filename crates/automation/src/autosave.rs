//! Durable MCP checkpoints, isolated from the desktop and other headless sessions.
//!
//! Bounded on disk: a checkpoint of a big project (at least [`THROTTLE_BYTES`]) is written at most
//! every [`THROTTLE_INTERVAL`] (and always when the session ends), each session keeps the
//! Auto-Save setting's maximum number of versions, its folder is emptied once the project is saved
//! normally, and folders of finished, saved sessions (or untouched for [`ABANDONED_AFTER`]) are
//! removed when the next session starts.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use effectcraft_engine::config::{FileOps, StdFiles};
use effectcraft_engine::{Session, autosave, prefs::Prefs};
use serde_json::{Value, json};

/// Checkpoints of at least this size are throttled (smaller ones are written after every change).
pub(crate) const THROTTLE_BYTES: u64 = 8 * 1024 * 1024;
/// The shortest time between two throttled checkpoints.
pub(crate) const THROTTLE_INTERVAL: Duration = Duration::from_secs(30);
/// A session folder untouched this long is removed when another session starts.
pub(crate) const ABANDONED_AFTER: Duration = Duration::from_secs(30 * 24 * 3600);

#[derive(Clone)]
pub(crate) struct AutoSave {
    folder: PathBuf,
    latest: Option<String>,
    error: Option<String>,
    previous: Vec<Value>,
    manifest: Option<Value>,
    /// When the last checkpoint was written and how big it was (for throttling).
    last_write: Option<(Instant, u64)>,
    throttle: (u64, Duration),
    /// A change is waiting for the next (throttled) checkpoint.
    pending: bool,
}

impl AutoSave {
    pub fn new(root: &Path, prefs: &Prefs) -> std::io::Result<Self> {
        let base = if prefs.auto_save.location == "custom" && !prefs.auto_save.folder.trim().is_empty() {
            PathBuf::from(prefs.auto_save.folder.trim()).join("MCP")
        } else {
            root.join(autosave::AUTOSAVE_FOLDER).join("MCP")
        };
        if base.to_str().is_none() {
            return Err(std::io::Error::other("the MCP auto-save folder must be valid UTF-8; set EFFECTCRAFT_CONFIG_DIR to a UTF-8 path"));
        }
        std::fs::create_dir_all(&base)?;
        prune(&base, SystemTime::now());
        let previous = previous_sessions(&base);
        let folder = unique_folder(&base)?;
        Ok(Self {
            folder,
            latest: None,
            error: None,
            previous,
            manifest: None,
            last_write: None,
            throttle: (THROTTLE_BYTES, THROTTLE_INTERVAL),
            pending: false,
        })
    }

    /// Throttle checkpoints of at least `bytes` to one per `interval` (tests use small values).
    #[cfg(test)]
    pub fn with_throttle(mut self, bytes: u64, interval: Duration) -> Self {
        self.throttle = (bytes, interval);
        self
    }

    pub fn folder(&self) -> &Path {
        &self.folder
    }

    pub fn info(&self) -> Value {
        json!({
            "enabled": true, "folder": self.folder.to_string_lossy(), "latestPath": self.latest,
            "error": self.error, "previousSessions": self.previous, "pending": self.pending,
            "recovery": "Recover with open_project {path: latestPath}, then save_project {path: a normal project file}. Previous sessions may still be running; they are never opened automatically.",
        })
    }

    pub fn checkpoint(&mut self, s: &mut Session, ended: bool) -> std::io::Result<()> {
        self.write(s, ended).map_err(|e| self.failed(e))
    }

    /// Record a failed checkpoint (reported by [`AutoSave::info`] until one succeeds).
    pub fn failed(&mut self, e: impl std::fmt::Display) -> std::io::Error {
        let message = format!("MCP auto-save in {} failed: {e}", self.folder.display());
        self.error = Some(message.clone());
        std::io::Error::other(message)
    }

    /// Whether the last checkpoint was big and recent enough to skip this one.
    fn throttled(&self) -> bool {
        let (bytes, interval) = self.throttle;
        self.last_write.is_some_and(|(at, size)| size >= bytes && at.elapsed() < interval)
    }

    fn write(&mut self, s: &mut Session, ended: bool) -> std::io::Result<()> {
        if !s.is_dirty() {
            // Saved normally (or nothing done): the recovery copies are no longer needed.
            self.pending = false;
            if self.latest.take().is_some() {
                self.manifest = None;
                s.autosave.last_path = None;
                match std::fs::remove_dir_all(&self.folder) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e),
                }
            }
            if ended {
                let _ = std::fs::remove_dir_all(&self.folder);
            }
            self.error = None;
            return Ok(());
        }
        if s.autosave.saved_revision != Some(s.revision) {
            if !ended && self.throttled() {
                self.pending = true;
                return Ok(());
            }
            std::fs::create_dir_all(&self.folder)?;
            s.autosave.background = false;
            let path = s.autosave_now().map_err(std::io::Error::other)?;
            let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            self.last_write = Some((Instant::now(), size));
            self.pending = false;
        }
        s.autosave_wait().map_err(std::io::Error::other)?;
        // Explicit file.autoSave and save-on-render-start use the same override. Keep their
        // successful latest path too, even when the revision was already checkpointed.
        if let Some(path) = &s.autosave.last_path {
            self.latest = Some(path.clone());
        }
        let record = json!({
            "pid": autosave::pid(), "project": s.path, "autosave": self.latest,
            "dirty": s.is_dirty(), "revision": s.revision, "ended": ended,
        });
        // Queries don't rewrite the same manifest or rotate project slots.
        if self.manifest.as_ref() != Some(&record) && self.latest.is_some() {
            StdFiles.write(&self.folder.join("session.json"), record.to_string().as_bytes())?;
            self.manifest = Some(record);
        }
        self.error = None;
        if ended && self.latest.is_none() {
            let _ = std::fs::remove_dir(&self.folder);
        }
        Ok(())
    }
}

/// Whether `name` is a session folder this module made (`<pid>-<nanos>-<n>`).
fn is_session_folder(name: &str) -> bool {
    let parts: Vec<&str> = name.split('-').collect();
    parts.len() == 3 && parts.iter().all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}

/// Remove session folders that are no longer needed: a finished session whose project was saved
/// (or that never checkpointed), and any folder untouched for [`ABANDONED_AFTER`]. A running
/// session is never finished, and recreates its folder if a later change needs it.
pub(crate) fn prune(base: &Path, now: SystemTime) {
    let Ok(entries) = std::fs::read_dir(base) else { return };
    for entry in entries.flatten().take(10_000) {
        let path = entry.path();
        let Ok(meta) = std::fs::symlink_metadata(&path) else { continue };
        if !meta.is_dir() || !entry.file_name().to_str().is_some_and(is_session_folder) {
            continue;
        }
        let manifest = path.join("session.json");
        let touched = std::fs::metadata(&manifest).or(Ok::<_, std::io::Error>(meta)).ok().and_then(|m| m.modified().ok());
        let abandoned = touched.and_then(|t| now.duration_since(t).ok()).is_some_and(|age| age >= ABANDONED_AFTER);
        let record: Option<Value> = std::fs::read(&manifest).ok().filter(|b| b.len() <= 64 * 1024).and_then(|b| serde_json::from_slice(&b).ok());
        let finished_clean = record.as_ref().is_some_and(|r| r.get("ended") == Some(&Value::Bool(true)) && r.get("dirty") == Some(&Value::Bool(false)));
        if abandoned || finished_clean {
            let _ = std::fs::remove_dir_all(&path);
        }
    }
}

fn unique_folder(base: &Path) -> std::io::Result<PathBuf> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos();
    for _ in 0..100 {
        let folder = base.join(format!("{}-{stamp}-{}", autosave::pid(), NEXT.fetch_add(1, Ordering::Relaxed)));
        match std::fs::create_dir(&folder) {
            Ok(()) => return Ok(folder),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::other("cannot reserve a unique MCP auto-save folder"))
}

fn previous_sessions(base: &Path) -> Vec<Value> {
    // Scan directory metadata with bounded memory, then read only the newest 200 small
    // manifests. Limiting read_dir before sorting could hide a newly killed session once
    // older folders accumulated. Recovery is explicit, so running sessions are safe to list.
    let Ok(entries) = std::fs::read_dir(base) else { return vec![] };
    let mut records = std::collections::BTreeSet::new();
    for entry in entries.flatten() {
        let path = entry.path().join("session.json");
        let Ok(meta) = path.metadata() else { continue };
        if !meta.is_file() || meta.len() > 64 * 1024 {
            continue;
        }
        records.insert((meta.modified().ok(), path));
        if records.len() > 200 {
            records.pop_first();
        }
    }
    records
        .into_iter()
        .rev()
        .filter_map(|(_, path)| {
            use std::io::Read;
            let mut bytes = Vec::new();
            std::fs::File::open(path).ok()?.take(64 * 1024 + 1).read_to_end(&mut bytes).ok()?;
            if bytes.len() > 64 * 1024 {
                return None;
            }
            let record: Value = serde_json::from_slice(&bytes).ok()?;
            let saved = record.get("autosave")?.as_str()?;
            if saved.len() > 4096 || record.get("dirty") != Some(&Value::Bool(true)) || !Path::new(saved).is_file() {
                return None;
            }
            let project = record.get("project").and_then(Value::as_str).filter(|p| p.len() <= 4096);
            Some(json!({"autosave": saved, "project": project, "dirty": true,
                "ended": record.get("ended").and_then(Value::as_bool).unwrap_or(false),
                "pid": record.get("pid").and_then(Value::as_u64),
            }))
        })
        .take(5)
        .collect()
}
