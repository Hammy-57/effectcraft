//! Where the catalog lives: a folder (the local backend) or the Worker in front of the bucket.

use std::path::PathBuf;

use crate::store::{valid_key, write_atomic};

/// Called once with a download's bytes or an error message.
pub type Done = Box<dyn FnOnce(Result<Vec<u8>, String>) + Send>;

/// A catalog location. Downloads are asynchronous (`done` may run on another thread, or before
/// `get` returns); uploads (publishing, desktop only) block.
pub trait Remote: Send + Sync {
    /// The bytes of `key` (`head.json`, `blobs/<hash>[.gz]`). A missing key is an error whose
    /// message starts with `not found`.
    fn get(&self, key: &str, done: Done);
    /// Upload `data` as `key`.
    fn put(&self, key: &str, data: &[u8]) -> Result<(), String> {
        let _ = (key, data);
        Err("this Motion Library location is read-only".into())
    }
    /// Where it is, for status replies (never includes a token).
    fn describe(&self) -> String;
}

/// A folder laid out as the bucket (`head.json`, `blobs/…`): the local backend, and a test double.
pub struct DirRemote {
    root: PathBuf,
}

impl DirRemote {
    pub fn new(root: impl Into<PathBuf>) -> DirRemote {
        DirRemote { root: root.into() }
    }
}

impl Remote for DirRemote {
    fn get(&self, key: &str, done: Done) {
        if !valid_key(key) {
            return done(Err(format!("bad key `{key}`")));
        }
        let path = self.root.join(key);
        done(match std::fs::read(&path) {
            Ok(bytes) => Ok(bytes),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(format!("not found: {key}")),
            Err(e) => Err(format!("{}: {e}", path.display())),
        })
    }
    fn put(&self, key: &str, data: &[u8]) -> Result<(), String> {
        if !valid_key(key) {
            return Err(format!("bad key `{key}`"));
        }
        write_atomic(&self.root.join(key), data)
    }
    fn describe(&self) -> String {
        self.root.display().to_string()
    }
}

/// [`Remote::get`], waiting for the answer (desktop: publishing, agents' `wait`). `Ok(None)` when
/// the key doesn't exist.
#[cfg(not(target_arch = "wasm32"))]
pub fn get_blocking(remote: &dyn Remote, key: &str, timeout: std::time::Duration) -> Result<Option<Vec<u8>>, String> {
    let (tx, rx) = std::sync::mpsc::channel();
    remote.get(
        key,
        Box::new(move |r| {
            let _ = tx.send(r);
        }),
    );
    match rx.recv_timeout(timeout) {
        Ok(Ok(bytes)) => Ok(Some(bytes)),
        Ok(Err(e)) if e.starts_with("not found") => Ok(None),
        Ok(Err(e)) => Err(e),
        Err(_) => Err(format!("{key}: no answer from {} in {} s", remote.describe(), timeout.as_secs())),
    }
}
