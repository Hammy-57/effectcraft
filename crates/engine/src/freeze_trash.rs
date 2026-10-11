//! Native retirement adapters. Linux follows the freedesktop Trash Specification 1.0
//! (https://specifications.freedesktop.org/trash/1.0/); Windows uses its shell Recycle Bin.
//! Neither adapter falls back to deleting cache files permanently.

#[cfg(any(target_os = "linux", target_os = "windows"))]
use std::path::{Path, PathBuf};

#[cfg(target_os = "linux")]
pub fn linux_files() -> Result<PathBuf, String> {
    let data = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .ok_or("cannot locate home Trash")?;
    if !data.is_absolute() {
        return Err("home Trash must be an absolute path".into());
    }
    Ok(data.join("Trash/files"))
}

#[cfg(target_os = "linux")]
pub fn linux_move(source: &Path) -> Result<PathBuf, String> {
    use std::io::Write;
    use std::os::unix::fs::DirBuilderExt;
    let files = linux_files()?;
    let trash = files.parent().ok_or("invalid Trash path")?;
    let info = trash.join("info");
    for dir in [trash, files.as_path(), info.as_path()] {
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true).mode(0o700);
        builder.create(dir).map_err(|e| e.to_string())?;
        if std::fs::symlink_metadata(dir).map_err(|e| e.to_string())?.file_type().is_symlink() {
            return Err("linked Trash directory; cache retained".into());
        }
    }
    let name = format!("EffectCraft-{}-{}", source.file_name().and_then(|v| v.to_str()).ok_or("invalid cache name")?, crate::freeze_runtime::now_millis());
    let target = files.join(&name);
    let descriptor = info.join(format!("{name}.trashinfo"));
    let original = source.canonicalize().map_err(|e| e.to_string())?;
    use std::os::unix::ffi::OsStrExt;
    let encoded: String = original
        .as_os_str()
        .as_bytes()
        .iter()
        .map(|b| if b.is_ascii_alphanumeric() || b"/-._~".contains(b) { char::from(*b).to_string() } else { format!("%{b:02X}") })
        .collect();
    let date = time::OffsetDateTime::now_local().unwrap_or_else(|_| time::OffsetDateTime::now_utc());
    let text = format!(
        "[Trash Info]\nPath={encoded}\nDeletionDate={:04}-{:02}-{:02}T{:02}:{:02}:{:02}\n",
        date.year(),
        u8::from(date.month()),
        date.day(),
        date.hour(),
        date.minute(),
        date.second()
    );
    let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(&descriptor).map_err(|e| e.to_string())?;
    file.write_all(text.as_bytes()).and_then(|_| file.sync_all()).map_err(|e| e.to_string())?;
    if let Err(e) = std::fs::create_dir(&target) {
        let _ = std::fs::remove_file(&descriptor);
        return Err(e.to_string());
    }
    if let Err(e) = std::fs::rename(source, &target) {
        let _ = std::fs::remove_dir(&target);
        let _ = std::fs::remove_file(&descriptor);
        return Err(format!("cache could not be moved to home Trash (including across filesystems); retained: {e}"));
    }
    Ok(target)
}

#[cfg(target_os = "linux")]
pub fn linux_remove_info(source: &Path) -> Result<(), String> {
    let files = linux_files()?;
    if source.parent() != Some(files.as_path()) {
        return Err("unexpected Trash source".into());
    }
    let name = source.file_name().and_then(|s| s.to_str()).ok_or("invalid Trash name")?;
    let info = files.parent().ok_or("invalid Trash path")?.join("info").join(format!("{name}.trashinfo"));
    match std::fs::remove_file(info) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}

#[cfg(target_os = "windows")]
pub fn windows_move(source: &Path) -> Result<PathBuf, String> {
    std::panic::catch_unwind(|| trash::delete(source))
        .map_err(|_| "Windows Recycle Bin failed; inspect storage before retrying".to_string())?
        .map_err(|e| e.to_string())?;
    // Every immutable version has a unique original directory. This locator does not depend
    // on the shell's opaque parsing name, and remains recoverable after an app restart.
    Ok(PathBuf::from(format!("windows-recycle:{}", source.to_string_lossy())))
}

#[cfg(target_os = "windows")]
pub fn windows_restore(root: &Path, directory: &str, locator: &str) -> Result<(), String> {
    let path = Path::new(directory);
    if path.parent() != Some(root) || locator != format!("windows-recycle:{directory}") {
        return Err("invalid cache recovery locator".into());
    }
    match std::fs::symlink_metadata(path) {
        Ok(_) => return Err("original cache location is occupied; nothing replaced".into()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.to_string()),
    }
    let canonical = root.canonicalize().map_err(|e| e.to_string())?;
    let items = std::panic::catch_unwind(trash::os_limited::list).map_err(|_| "cannot read Windows Recycle Bin".to_string())?.map_err(|e| e.to_string())?;
    if items.len() > 100_000 {
        return Err("Recycle Bin listing exceeds its verification budget".into());
    }
    let item = items
        .into_iter()
        .filter(|i| Some(i.name.as_os_str()) == path.file_name() && i.original_parent.canonicalize().is_ok_and(|p| p == canonical))
        .max_by_key(|i| i.time_deleted)
        .ok_or("this cache is no longer in the Recycle Bin")?;
    std::panic::catch_unwind(|| trash::os_limited::restore_all([item])).map_err(|_| "Windows cache restoration failed".to_string())?.map_err(|e| e.to_string())
}
