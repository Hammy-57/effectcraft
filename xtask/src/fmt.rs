//! `cargo xtask fmt [--check]`: rustfmt every Rust source file of the workspace.
//!
//! `cargo fmt` only reaches files through `mod` declarations it can read, and the modules that
//! `effectcraft-buildgen` declares from `build.rs` (commands, effects, panels, test modules…)
//! live in generated `include!`s it does not expand. So this formats each tracked (or new,
//! not ignored) `.rs` file under `crates/`, `apps/` and `xtask/` directly.

use std::process::Command;

/// Files per rustfmt invocation (keeps command lines short on Windows).
const BATCH: usize = 200;

/// `run(["--check"])`, as a plain fn for the CI step lists.
pub fn check() -> Result<(), String> {
    run(&["--check".into()])
}

pub fn run(args: &[String]) -> Result<(), String> {
    let check = args.iter().any(|a| a == "--check");
    let root = crate::root();
    let out = Command::new("git")
        .current_dir(&root)
        .args(["ls-files", "--cached", "--others", "--exclude-standard", "--", "crates", "apps", "xtask"])
        .output()
        .map_err(|e| format!("git ls-files: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).into());
    }
    let mut files: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|f| f.ends_with(".rs") && !f.split('/').any(|seg| seg == "target" || seg.starts_with("target-")))
        .filter(|f| root.join(f).is_file())
        .map(str::to_string)
        .collect();
    files.sort();
    files.dedup();
    let skipped = skipped_modules(&root, &files);
    files.retain(|f| !skipped.contains(f));
    let rustfmt = std::env::var("RUSTFMT").unwrap_or_else(|_| "rustfmt".into());
    eprintln!("$ {rustfmt} --edition 2024{} <{} files>", if check { " --check" } else { "" }, files.len());
    let mut failed = false;
    for chunk in files.chunks(BATCH) {
        let mut cmd = Command::new(&rustfmt);
        cmd.current_dir(&root).args(["--edition", "2024"]);
        if check {
            cmd.arg("--check");
        }
        let st = cmd.args(chunk).status().map_err(|e| format!("{rustfmt}: {e}"))?;
        failed |= !st.success();
    }
    match (failed, check) {
        (false, _) => {
            println!("fmt: ok ({} files)", files.len());
            Ok(())
        }
        (true, true) => Err("fmt: files above need formatting; run `cargo xtask fmt`".into()),
        (true, false) => Err("fmt: rustfmt failed".into()),
    }
}

/// Files of modules declared `#[rustfmt::skip] mod x;` (rustfmt honours that only when it
/// reaches the file through the declaration, not when it is given the file).
fn skipped_modules(root: &std::path::Path, files: &[String]) -> Vec<String> {
    let mut out = vec![];
    for f in files {
        let Ok(text) = std::fs::read_to_string(root.join(f)) else { continue };
        let (dir, stem) = f.rsplit_once('/').unwrap_or(("", f.as_str()));
        let stem = stem.trim_end_matches(".rs");
        // Children of `lib.rs` / `main.rs` / `mod.rs` live next to it, others in `<stem>/`.
        let base = if matches!(stem, "lib" | "main" | "mod") { dir.to_string() } else { format!("{dir}/{stem}") };
        let mut skip = false;
        for line in text.lines().map(str::trim) {
            if line == "#[rustfmt::skip]" {
                skip = true;
                continue;
            }
            if skip {
                let decl = line.strip_prefix("pub(crate) ").or_else(|| line.strip_prefix("pub ")).unwrap_or(line);
                if let Some(name) = decl.strip_prefix("mod ").and_then(|r| r.strip_suffix(';')) {
                    out.push(format!("{base}/{name}.rs"));
                    out.push(format!("{base}/{name}/mod.rs"));
                }
            }
            // Other attributes may sit between `#[rustfmt::skip]` and the item.
            skip = skip && line.starts_with("#[");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn rustfmt_skip_modules_are_left_alone() {
        let skipped = super::skipped_modules(&crate::root(), &["crates/hevcenc/src/lib.rs".to_string()]);
        assert!(skipped.contains(&"crates/hevcenc/src/tables.rs".to_string()));
    }
}
