//! New-directory package writer; project-last completion, not atomic directory visibility.
use crate::Services;
use std::path::{Component, Path, PathBuf};

pub(super) struct NativeOutput<'a> {
    folder: PathBuf,
    services: &'a dyn Services,
    committed: bool,
}

impl<'a> NativeOutput<'a> {
    pub(super) fn create(folder: &Path, services: &'a dyn Services) -> std::io::Result<Self> {
        // Existing files, empty directories and symlinks are all refused.
        std::fs::create_dir(folder)?;
        Ok(Self { folder: folder.to_owned(), services, committed: false })
    }

    fn destination(&self, relative: &str) -> std::io::Result<PathBuf> {
        let path = Path::new(relative);
        if relative.is_empty() || relative.contains('\\') || path.components().any(|part| !matches!(part, Component::Normal(_))) {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "package path must be a relative path without traversal"));
        }
        let destination = self.folder.join(path);
        // Generated package names are unique; catch planner bugs before overwriting output.
        if destination.symlink_metadata().is_ok() {
            return Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, "duplicate package destination"));
        }
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)?;
        }
        Ok(destination)
    }

    pub(super) fn write(&self, relative: &str, bytes: &[u8]) -> std::io::Result<()> {
        let destination = self.destination(relative)?;
        self.services.write_file(&destination.to_string_lossy(), bytes)
    }

    pub(super) fn copy(&self, source: &str, relative: &str) -> std::io::Result<()> {
        let destination = self.destination(relative)?;
        self.services.copy_file(source, &destination.to_string_lossy())
    }

    pub(super) fn commit(mut self) {
        self.committed = true;
    }

    pub(super) fn abort(mut self) -> std::io::Result<()> {
        let result = std::fs::remove_dir_all(&self.folder);
        self.committed = true;
        result
    }
}

impl Drop for NativeOutput<'_> {
    fn drop(&mut self) {
        if !self.committed {
            // Error-path fallback. Explicit abort can report cleanup failure to the caller.
            let _ = std::fs::remove_dir_all(&self.folder);
        }
    }
}

// Integrate only when Services explicitly supports native collection (not merely !wasm).
// All generated names should use cross-platform safe basenames and case-insensitive uniqueness.
// No existing user tree is modified. This does not defend against a hostile process swapping
// directories/symlinks during the operation; existing Services has the same filesystem boundary.
