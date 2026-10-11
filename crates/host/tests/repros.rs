//! Regression tests as data: every `tests/repros/*.jsonl` (repository root) is a replay script
//! (`effectcraft_engine::replay`, docs/agents.md "Repro scripts") run against a fully wired
//! headless session, like `effectcraft-cli replay`. A crash fix's regression test is a `.jsonl`
//! dropped there: it must replay without a panic and meet its expectations, and its `golden`
//! frames must still match.
//!
//! `EFFECTCRAFT_REPROS` adds more folders (separated like `PATH`), for corpora that can't be
//! committed, such as users' own projects kept in the gitignored `plan/repros/`.
//! `EFFECTCRAFT_BLESS=1` writes the `golden` frames instead of comparing them (`cargo xtask
//! repros --bless`, after a change that is meant to alter a render).

use std::path::{Path, PathBuf};

use effectcraft_engine::replay;

fn repros_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/repros")
}

/// The `.jsonl` scripts in `dir` and its subfolders (one level: a project's folder).
fn scripts(dir: &Path) -> Vec<PathBuf> {
    let mut out = vec![];
    let Ok(rd) = std::fs::read_dir(dir) else { return out };
    for p in rd.filter_map(|e| e.ok()).map(|e| e.path()) {
        if p.extension().is_some_and(|x| x == "jsonl") {
            out.push(p);
        } else if p.is_dir() {
            out.extend(
                std::fs::read_dir(&p).into_iter().flatten().filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "jsonl")),
            );
        }
    }
    out
}

#[test]
fn every_repro_replays_cleanly() {
    let mut files = scripts(&repros_dir());
    assert!(!files.is_empty(), "no repros in {}", repros_dir().display());
    if let Some(extra) = std::env::var_os("EFFECTCRAFT_REPROS") {
        for dir in std::env::split_paths(&extra) {
            let found = scripts(&dir);
            assert!(!found.is_empty(), "EFFECTCRAFT_REPROS: no .jsonl scripts in {}", dir.display());
            files.extend(found);
        }
    }
    files.sort();
    let bless = std::env::var("EFFECTCRAFT_BLESS").is_ok_and(|v| v == "1");
    let mut failures = vec![];
    for f in &files {
        let script = std::fs::read_to_string(f).unwrap();
        let mut s = effectcraft_host::session();
        let opts = replay::Options { keep_going: false, base_dir: f.parent().map(Path::to_path_buf), bless };
        let r = replay::run(&mut s, &script, &opts);
        if !r.ok {
            failures.push(format!("{}: {}", f.display(), serde_json::to_string(&r.first_failure).unwrap()));
        }
    }
    assert!(failures.is_empty(), "{} of {} repros failed:\n{}", failures.len(), files.len(), failures.join("\n"));
}
