//! Self-registering modules (see `effectcraft-buildgen`): every `src/*.rs` file is a module of
//! the crate (`tests*.rs` / `*_tests.rs` are `#[cfg(test)]`), and every `src/commands/*.rs` file
//! is a command module whose `specs()` is registered. Adding a command is adding a file.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use effectcraft_buildgen::Modules;

fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git").current_dir(dir).args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout).ok().map(|s| s.trim().to_string())
}

fn track_existing(path: &Path) {
    if path.exists() {
        println!("cargo:rerun-if-changed={}", path.display());
    }
}

fn build_input(file: &str) -> bool {
    // Test-only changes do not change the executable's provenance; Cargo tracks tests itself.
    let name = Path::new(file).file_name().and_then(|name| name.to_str()).unwrap_or("");
    if file.contains("/tests/") || file.contains("/benches/") || name == "tests.rs" || name.starts_with("tests_") || name.ends_with("_tests.rs") {
        return false;
    }
    if matches!(file, "Cargo.toml" | "Cargo.lock" | "rust-toolchain" | "rust-toolchain.toml" | ".cargo/config" | ".cargo/config.toml") {
        return true;
    }
    let source_folder = ["crates/", "extensions/", "packaging/", "scripts/", "contributors/"].iter().any(|prefix| file.starts_with(prefix));
    source_folder
        && Path::new(file).extension().and_then(|ext| ext.to_str()).is_some_and(|ext| {
            matches!(ext, "rs" | "toml" | "lock" | "wgsl" | "glsl" | "hlsl" | "js" | "jsx" | "ts" | "tsx" | "css" | "html" | "py" | "sh" | "json")
        })
}

fn build_identity() {
    let manifest = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap_or_default());
    let root = git(&manifest, &["rev-parse", "--show-toplevel"]).map(PathBuf::from).unwrap_or(manifest);
    let commit = git(&root, &["rev-parse", "HEAD"])
        .filter(|s| matches!(s.len(), 40 | 64) && s.bytes().all(|b| b.is_ascii_hexdigit()))
        .unwrap_or_else(|| "unknown".into());
    let dirty = git(&root, &["status", "--porcelain", "--untracked-files=normal"]).map(|s| if s.is_empty() { "false" } else { "true" }).unwrap_or("unknown");
    // --git-path resolves both a normal checkout and linked-worktree/common-dir refs.
    for name in ["HEAD", "index", "packed-refs"] {
        if let Some(path) = git(&root, &["rev-parse", "--path-format=absolute", "--git-path", name]) {
            track_existing(Path::new(&path));
        }
    }
    if let Some(reference) = git(&root, &["symbolic-ref", "--quiet", "HEAD"])
        && let Some(path) = git(&root, &["rev-parse", "--path-format=absolute", "--git-path", &reference])
    {
        let path = Path::new(&path);
        track_existing(path);
        if !path.exists()
            && let Some(refs) = git(&root, &["rev-parse", "--path-format=absolute", "--git-path", "refs"])
        {
            let refs = Path::new(&refs);
            let mut parent = path.parent();
            while let Some(dir) = parent.filter(|dir| dir.starts_with(refs)) {
                if dir.exists() {
                    track_existing(dir);
                    break;
                }
                parent = dir.parent();
            }
        }
    }
    // Refresh provenance for executable source/build inputs without rebuilding for documentation.
    if let Ok(output) = Command::new("git").current_dir(&root).args(["ls-files", "-z"]).output()
        && output.status.success()
        && let Ok(files) = String::from_utf8(output.stdout)
    {
        for file in files.split('\0').filter(|file| build_input(file)) {
            track_existing(&root.join(file));
        }
    }
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
    let target = std::env::var("TARGET").unwrap_or_else(|_| "unknown".into());
    let profile = std::env::var("PROFILE").unwrap_or_else(|_| "unknown".into());
    println!("cargo:rustc-env=EFFECTCRAFT_BUILD_COMMIT={commit}");
    println!("cargo:rustc-env=EFFECTCRAFT_BUILD_DIRTY={dirty}");
    println!("cargo:rustc-env=EFFECTCRAFT_BUILD_ID={commit}-{dirty}-{stamp}");
    println!("cargo:rustc-env=EFFECTCRAFT_BUILD_TARGET={target}");
    println!("cargo:rustc-env=EFFECTCRAFT_BUILD_PROFILE={profile}");
}

fn main() -> Result<(), String> {
    build_identity();
    Modules::new("src", "src/lib.rs", "modules.rs").visibility("pub").registry("COMMAND_MODULES", "specs", "fn() -> Vec<crate::CommandSpec>").generate()?;
    Modules::new("src/commands", "src/commands/mod.rs", "commands.rs").registry("COMMAND_MODULES", "specs", "fn() -> Vec<crate::CommandSpec>").generate()
}
