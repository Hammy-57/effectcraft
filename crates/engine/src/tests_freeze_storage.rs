use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use effectcraft_project::freeze::Version;
use effectcraft_render::{Image, RenderOpts};
use effectcraft_time::FrameRate;

use crate::freeze_storage::{self as storage, TEN_HOURS};

fn fixture() -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "effectcraft-freeze-storage-{}-{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    std::fs::create_dir(&root).unwrap();
    root
}

fn version(root: &Path, id: u64) -> Version {
    let dir = root.join(format!("1-1-{id}"));
    std::fs::create_dir(&dir).unwrap();
    std::fs::write(dir.join("owner.json"), r#"{"owner":"effectcraft-smart-freeze","version":1}"#).unwrap();
    let bytes = effectcraft_render::freeze::write_frame(&dir.join("0.ecfreeze"), &Image::new(1, 1)).unwrap();
    Version {
        directory: dir.to_string_lossy().into_owned(),
        identity: id.to_string(),
        draft: false,
        width: 1,
        height: 1,
        frames: 1,
        bytes,
        created_unix: 1,
        render_options: serde_json::to_string(&RenderOpts::default()).unwrap(),
        frame_rate: FrameRate::FPS_30,
        sample: None,
        reused_frames: 0,
    }
}

#[test]
fn freeze_retirement_grace_starts_when_replaced_and_keeps_latest() {
    let root = fixture();
    let trash = fixture();
    let (old, new) = (version(&root, 1), version(&root, 2));
    storage::record(&root, 1, &old, 1).unwrap();
    storage::record(&root, 1, &new, 100).unwrap();
    let mover = |path: &Path| {
        let to = trash.join(path.file_name().unwrap());
        std::fs::rename(path, &to).map_err(|e| e.to_string())?;
        Ok(to)
    };
    assert!(storage::retire(&root, &BTreeSet::new(), TEN_HOURS + 99, false, mover).unwrap().retired.is_empty());
    let report = storage::retire(&root, &BTreeSet::new(), TEN_HOURS + 100, false, mover).unwrap();
    assert_eq!(report.retired.as_slice(), std::slice::from_ref(&old.directory));
    assert!(Path::new(&new.directory).exists());
    assert!(trash.join("1-1-1/recovery.json").exists());
    assert!(storage::retire(&root, &BTreeSet::new(), TEN_HOURS + 101, true, mover).unwrap().retired.is_empty());
    std::fs::remove_dir_all(root).unwrap();
    std::fs::remove_dir_all(trash).unwrap();
}

#[test]
fn freeze_retirement_protects_current_and_retries_failed_moves() {
    let root = fixture();
    let trash = fixture();
    let (old, new) = (version(&root, 1), version(&root, 2));
    storage::record(&root, 1, &old, 1).unwrap();
    storage::record(&root, 1, &new, 2).unwrap();
    let protected = BTreeSet::from([old.directory.clone()]);
    assert!(storage::retire(&root, &protected, 3, true, |_| Err("must not move protected content".into())).unwrap().errors.is_empty());
    let failed = storage::retire(&root, &BTreeSet::new(), 3, true, |_| Err("test failure".into())).unwrap();
    assert_eq!(failed.errors, ["test failure"]);
    assert!(Path::new(&old.directory).exists());
    let retried = storage::retire(&root, &BTreeSet::new(), 4, true, |path| {
        let to = trash.join(path.file_name().unwrap());
        std::fs::rename(path, &to).map_err(|e| e.to_string())?;
        Ok(to)
    })
    .unwrap();
    assert_eq!(retried.retired.len(), 1);
    std::fs::remove_dir_all(root).unwrap();
    std::fs::remove_dir_all(trash).unwrap();
}

#[test]
fn freeze_abandoned_cleanup_requires_terminal_old_never_attached_output() {
    let root = fixture();
    let trash = fixture();
    let abandoned = version(&root, 1);
    let active = version(&root, 2);
    let attached = version(&root, 3);
    let uncertain = version(&root, 4);
    std::fs::write(Path::new(&uncertain.directory).join("terminal.json"), r#"{"finishedUnix":1}"#).unwrap();
    std::fs::write(Path::new(&uncertain.directory).join("version.json"), serde_json::to_vec(&uncertain).unwrap()).unwrap();
    std::fs::write(Path::new(&abandoned.directory).join("terminal.json"), r#"{"finishedUnix":1}"#).unwrap();
    std::fs::write(Path::new(&attached.directory).join("terminal.json"), r#"{"finishedUnix":1}"#).unwrap();
    storage::record(&root, 3, &attached, 1).unwrap();
    let report = storage::maintain(&root, &BTreeSet::new(), 86402, false, 0, false, |path| {
        let to = trash.join(path.file_name().unwrap());
        std::fs::rename(path, &to).map_err(|e| e.to_string())?;
        Ok(to)
    })
    .unwrap();
    assert_eq!(report.retired, [abandoned.directory]);
    assert!(Path::new(&active.directory).exists());
    assert!(Path::new(&attached.directory).exists());
    assert!(Path::new(&uncertain.directory).exists(), "completed outputs without a ledger might have been attached");
    assert!(report.bytes > 0, "the budget is soft: active and attached data stay protected");
    std::fs::remove_dir_all(root).unwrap();
    std::fs::remove_dir_all(trash).unwrap();
}

#[test]
fn freeze_adopts_previous_drafts_without_restarting_retirement_grace() {
    let root = fixture();
    let mut draft = version(&root, 1);
    draft.draft = true;
    storage::adopt_previous(&root, 9, std::slice::from_ref(&draft), 100).unwrap();
    storage::adopt_previous(&root, 9, std::slice::from_ref(&draft), 200).unwrap();
    let history = storage::history(&root).unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].superseded_at, Some(100));
    assert_eq!(history[0].version, draft);
    std::fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[test]
fn freeze_cleanup_never_follows_links_or_removes_unexpected_files() {
    let root = fixture();
    let outside = fixture();
    let old = version(&root, 1);
    let new = version(&root, 2);
    storage::record(&root, 1, &old, 1).unwrap();
    storage::record(&root, 1, &new, 2).unwrap();
    let important = outside.join("source.txt");
    std::fs::write(&important, b"source media").unwrap();
    std::os::unix::fs::symlink(&important, Path::new(&old.directory).join("unexpected-link")).unwrap();
    let result = storage::retire(&root, &BTreeSet::new(), 3, true, |_| Err("mover should not be reached".into())).unwrap();
    assert!(result.errors.iter().any(|e| e.contains("unexpected")));
    assert!(Path::new(&old.directory).exists());
    assert_eq!(std::fs::read(important).unwrap(), b"source media");
    std::fs::remove_dir_all(root).unwrap();
    std::fs::remove_dir_all(outside).unwrap();
}

#[cfg(target_os = "macos")]
#[test]
#[ignore = "requires native macOS Trash access; run explicitly against generated content"]
fn freeze_native_trash_roundtrip() {
    let root = fixture();
    let old = version(&root, 1);
    let new = version(&root, 2);
    storage::record(&root, 1, &old, 1).unwrap();
    storage::record(&root, 1, &new, 2).unwrap();
    let result = storage::retire(&root, &BTreeSet::new(), 3, true, storage::move_to_trash).unwrap();
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert_eq!(result.retired.len(), 1);
    storage::restore(&root, &old.directory).unwrap();
    assert!(Path::new(&old.directory).join("0.ecfreeze").exists());
    assert!(storage::history(&root).unwrap().iter().all(|e| e.trash_path.is_none()));
    std::fs::remove_dir_all(root).unwrap();
}
