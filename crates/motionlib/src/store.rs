//! The local cache: blobs by content hash, plus the adopted [`crate::Head`].

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// A key is relative path segments of `[A-Za-z0-9 ._-]`, none of them `.` or `..`.
pub fn valid_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 400
        && key
            .split('/')
            .all(|seg| !seg.is_empty() && seg != "." && seg != ".." && seg.chars().all(|c| c.is_alphanumeric() || matches!(c, '.' | '-' | '_' | ' ')))
}

/// Where downloaded blobs are kept.
pub trait Store: Send + Sync {
    fn get(&self, key: &str) -> Option<Vec<u8>>;
    fn put(&self, key: &str, data: &[u8]) -> Result<(), String>;
    fn has(&self, key: &str) -> bool;
    /// The file holding `key`, when the store is a folder (importers read files by path).
    fn path(&self, key: &str) -> Option<PathBuf> {
        let _ = key;
        None
    }
    /// Short description for status replies.
    fn describe(&self) -> String;
}

/// A folder (desktop): `<root>/<key>`.
pub struct DirStore {
    root: PathBuf,
}

impl DirStore {
    pub fn new(root: impl Into<PathBuf>) -> DirStore {
        DirStore { root: root.into() }
    }
    pub fn root(&self) -> &Path {
        &self.root
    }
    fn file(&self, key: &str) -> Option<PathBuf> {
        valid_key(key).then(|| self.root.join(key))
    }
}

/// Write `data` to `path` through a temporary file and a rename (no half-written cache file).
pub(crate) fn write_atomic(path: &Path, data: &[u8]) -> Result<(), String> {
    let dir = path.parent().ok_or_else(|| format!("{}: no folder", path.display()))?;
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let tmp = dir.join(format!(".{name}.{}.part", std::process::id()));
    std::fs::write(&tmp, data).map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("{}: {e}", path.display())
    })
}

impl Store for DirStore {
    fn get(&self, key: &str) -> Option<Vec<u8>> {
        std::fs::read(self.file(key)?).ok()
    }
    fn put(&self, key: &str, data: &[u8]) -> Result<(), String> {
        let path = self.file(key).ok_or_else(|| format!("bad cache key `{key}`"))?;
        write_atomic(&path, data)
    }
    fn has(&self, key: &str) -> bool {
        self.file(key).is_some_and(|p| p.is_file())
    }
    fn path(&self, key: &str) -> Option<PathBuf> {
        self.file(key).filter(|p| p.is_file())
    }
    fn describe(&self) -> String {
        self.root.display().to_string()
    }
}

/// In memory (the browser build, tests).
#[derive(Default)]
pub struct MemStore {
    files: Mutex<BTreeMap<String, Vec<u8>>>,
}

impl Store for MemStore {
    fn get(&self, key: &str) -> Option<Vec<u8>> {
        self.files.lock().ok()?.get(key).cloned()
    }
    fn put(&self, key: &str, data: &[u8]) -> Result<(), String> {
        if !valid_key(key) {
            return Err(format!("bad cache key `{key}`"));
        }
        self.files.lock().map_err(|_| "cache lock poisoned".to_string())?.insert(key.into(), data.to_vec());
        Ok(())
    }
    fn has(&self, key: &str) -> bool {
        self.files.lock().is_ok_and(|f| f.contains_key(key))
    }
    fn describe(&self) -> String {
        "memory".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys() {
        assert!(valid_key("head.json") && valid_key("blobs/ab12/My clip.png"));
        for bad in ["", "../x", "a//b", "/abs", "a/./b", "a\\b", "x?"] {
            assert!(!valid_key(bad), "{bad}");
        }
    }

    #[test]
    fn dir_store_round_trip() {
        let dir = std::env::temp_dir().join(format!("ec-motionlib-store-{}", std::process::id()));
        let s = DirStore::new(&dir);
        assert!(!s.has("blobs/a/b.png"));
        s.put("blobs/a/b.png", b"png").unwrap();
        assert_eq!(s.get("blobs/a/b.png").unwrap(), b"png");
        assert!(s.path("blobs/a/b.png").unwrap().ends_with("blobs/a/b.png"));
        assert!(s.put("../escape", b"x").is_err());
        let _ = std::fs::remove_dir_all(dir);
    }
}
