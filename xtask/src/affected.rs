//! `cargo xtask ci --affected [--base REV] [--dry-run]`: the CI gates, run only for the crates a
//! change can affect (docs/testing.md).
//!
//! 1. Changed files: everything that differs from the base (default: the merge-base with `main`),
//!    committed, staged, unstaged or untracked.
//! 2. Files map to the workspace package whose directory holds them, and to every package whose
//!    sources point at them with a `../` path (`include_bytes!("../../../assets/fonts/…")`,
//!    `env!("CARGO_MANIFEST_DIR")` + `"/../../docs/effects.md"`, …).
//! 3. Affected = those packages, every package that depends on them (transitively, through
//!    normal and build dependencies) and every package that has one of them as a dev-dependency.
//! 4. Workspace-wide inputs (root `Cargo.toml`, `Cargo.lock`, `.cargo/`, `xtask/`, the toolchain
//!    file, `clippy.toml`) fall back to the full `cargo xtask ci`.

use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use serde_json::Value;

/// A workspace package, as far as change mapping cares.
#[derive(Debug, Clone, Default)]
pub struct Pkg {
    pub name: String,
    /// Package directory relative to the workspace root, `/`-separated, no trailing slash.
    pub dir: String,
    /// Workspace (path) dependencies: (package name, is a dev-dependency).
    pub deps: Vec<(String, bool)>,
    /// Has a library target with doctests (`cargo test --doc` runs for it).
    pub has_lib: bool,
    /// Repo paths outside `dir` that the sources read (files or directories).
    pub inputs: Vec<String>,
}

/// Changes to these force the full CI: they can change every crate's build.
pub fn full_ci_reason(path: &str) -> Option<&'static str> {
    match path {
        "Cargo.toml" => Some("workspace Cargo.toml changed"),
        "Cargo.lock" => Some("Cargo.lock changed"),
        "rust-toolchain" | "rust-toolchain.toml" => Some("toolchain changed"),
        "clippy.toml" => Some("clippy.toml changed"),
        _ if path.starts_with(".cargo/") => Some(".cargo/ changed"),
        _ if path.starts_with("xtask/") => Some("xtask changed"),
        _ => None,
    }
}

/// Files the `assets` check looks at (see `assets()` in main.rs).
pub fn is_asset_related(path: &str) -> bool {
    path == "ATTRIBUTION.md" || path.ends_with(".attribution") || crate::is_asset(path)
}

fn under(path: &str, dir: &str) -> bool {
    dir.is_empty() || path == dir || path.strip_prefix(dir).is_some_and(|r| r.starts_with('/'))
}

/// The package whose directory holds `path` (the innermost one).
pub fn owner<'a>(path: &str, pkgs: &'a [Pkg]) -> Option<&'a Pkg> {
    pkgs.iter().filter(|p| !p.dir.is_empty() && under(path, &p.dir)).max_by_key(|p| p.dir.len())
}

/// Packages whose sources read `path` from outside their own directory.
pub fn readers<'a>(path: &'a str, pkgs: &'a [Pkg]) -> impl Iterator<Item = &'a Pkg> + 'a {
    pkgs.iter().filter(move |p| p.inputs.iter().any(|i| under(path, i)))
}

/// `direct` plus everything that depends on it: transitively through normal/build edges, then
/// one hop through dev-dependency edges (a dev-dependent's tests rerun, its dependents don't).
pub fn expand(direct: &BTreeSet<String>, pkgs: &[Pkg]) -> BTreeSet<String> {
    let mut out = direct.clone();
    let mut stack: Vec<String> = direct.iter().cloned().collect();
    while let Some(n) = stack.pop() {
        for p in pkgs {
            if !out.contains(&p.name) && p.deps.iter().any(|(d, dev)| !dev && *d == n) {
                out.insert(p.name.clone());
                stack.push(p.name.clone());
            }
        }
    }
    let dev: Vec<String> = pkgs.iter().filter(|p| p.deps.iter().any(|(d, dev)| *dev && out.contains(d))).map(|p| p.name.clone()).collect();
    out.extend(dev);
    out
}

/// What `ci --affected` will do for a set of changed files.
#[derive(Debug, Default, PartialEq)]
pub struct Plan {
    /// Set when the full CI must run instead.
    pub full: Option<String>,
    /// Packages that hold or read a changed file.
    pub direct: BTreeSet<String>,
    /// `direct` and its dependents: clippy and tests run for these.
    pub affected: BTreeSet<String>,
    /// Whether the assets check runs.
    pub assets: bool,
    /// Changed files no package reads (docs, CI configs, packaging…).
    pub unmapped: Vec<String>,
}

pub fn plan(files: &[String], pkgs: &[Pkg]) -> Plan {
    let mut plan = Plan::default();
    for f in files {
        if let Some(why) = full_ci_reason(f) {
            plan.full = Some(format!("{why} ({f})"));
            return plan;
        }
        plan.assets |= is_asset_related(f);
        let mut hit = false;
        for p in owner(f, pkgs).into_iter().chain(readers(f, pkgs)) {
            plan.direct.insert(p.name.clone());
            hit = true;
        }
        if !hit {
            plan.unmapped.push(f.clone());
        }
    }
    plan.affected = expand(&plan.direct, pkgs);
    plan
}

/// Normalise `base/rel` (both `/`-separated, relative to the repo root). None if it leaves the repo.
pub fn join_rel(base: &str, rel: &str) -> Option<String> {
    let mut parts: Vec<&str> = base.split('/').filter(|s| !s.is_empty() && *s != ".").collect();
    for c in rel.split('/') {
        match c {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            c => parts.push(c),
        }
    }
    Some(parts.join("/"))
}

/// The `../…` paths in a line of Rust: the part of each string literal from its first `../`,
/// cut back to a directory when a format placeholder follows (`"{}/../model/fixtures/{n}"`).
pub fn rel_paths(line: &str) -> Vec<String> {
    let mut out = vec![];
    for lit in line.split('"').skip(1).step_by(2) {
        let Some(i) = lit.find("../") else { continue };
        let Some(rest) = lit.get(i..) else { continue };
        let p = match rest.find('{') {
            Some(j) => rest.get(..j).and_then(|s| s.rfind('/').and_then(|k| s.get(..k))).unwrap_or(""),
            None => rest,
        };
        let p = p.trim_end_matches('/');
        if !p.is_empty() {
            out.push(p.to_string());
        }
    }
    out
}

/// Repo paths outside `pkg_dir` that a source file at `file` (both repo-relative) points at.
/// A literal may be relative to the file (`include_bytes!`) or to the manifest dir
/// (`CARGO_MANIFEST_DIR`), so both are tried and only existing paths are kept.
pub fn inputs_of(file: &str, pkg_dir: &str, text: &str, exists: &dyn Fn(&str) -> bool) -> BTreeSet<String> {
    let file_dir = file.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
    let mut out = BTreeSet::new();
    for line in text.lines().filter(|l| l.contains("../")) {
        let t = line.trim_start();
        if t.starts_with("//") && !t.starts_with("///") && !t.starts_with("//!") {
            continue;
        }
        for rel in rel_paths(line) {
            for base in [file_dir, pkg_dir] {
                let Some(p) = join_rel(base, &rel) else { continue };
                if p.is_empty() || p == "target" || p.starts_with("target/") || under(&p, pkg_dir) || !exists(&p) {
                    continue;
                }
                out.insert(p);
            }
        }
    }
    out
}

fn rs_files(dir: &Path, out: &mut Vec<std::path::PathBuf>, depth: u32) {
    if depth > 32 {
        return;
    }
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path();
        let name = e.file_name();
        if p.is_dir() {
            if name != "target" && name != ".git" {
                rs_files(&p, out, depth + 1);
            }
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

fn rel_to(root: &Path, p: &Path) -> String {
    p.strip_prefix(root).unwrap_or(p).components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect::<Vec<_>>().join("/")
}

/// Workspace packages (xtask excluded) with their path dependencies and outside inputs.
pub fn packages(root: &Path, md: &Value) -> Vec<Pkg> {
    let ws = md["workspace_root"].as_str().map(Path::new).unwrap_or(root);
    let names: BTreeSet<&str> = md["packages"].as_array().into_iter().flatten().filter_map(|p| p["name"].as_str()).collect();
    let mut pkgs = vec![];
    for p in md["packages"].as_array().into_iter().flatten() {
        let Some(name) = p["name"].as_str().filter(|n| *n != "xtask") else { continue };
        let Some(dir) = p["manifest_path"].as_str().and_then(|m| Path::new(m).parent()) else { continue };
        let deps = p["dependencies"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|d| !d["path"].is_null())
            .filter_map(|d| d["name"].as_str().filter(|n| names.contains(n)).map(|n| (n.to_string(), d["kind"] == "dev")))
            .collect();
        let lib_kind = |k: &Value| ["lib", "rlib", "dylib", "proc-macro"].iter().any(|l| k == *l);
        let has_lib = p["targets"].as_array().into_iter().flatten().any(|t| t["doctest"] != false && t["kind"].as_array().into_iter().flatten().any(lib_kind));
        let rel = rel_to(ws, dir);
        let mut files = vec![];
        rs_files(dir, &mut files, 0);
        let exists = |p: &str| ws.join(p).exists();
        let mut inputs = BTreeSet::new();
        for f in files {
            if let Ok(text) = std::fs::read_to_string(&f) {
                inputs.extend(inputs_of(&rel_to(ws, &f), &rel, &text, &exists));
            }
        }
        pkgs.push(Pkg { name: name.to_string(), dir: rel, deps, has_lib, inputs: inputs.into_iter().collect() });
    }
    pkgs
}

fn git(root: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git").arg("-C").arg(root).args(args).output().map_err(|e| format!("git: {e}"))?;
    if !out.status.success() {
        return Err(format!("git {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The base commit: `--base REV` or the merge-base of HEAD with `main` (or `origin/main`).
fn base(root: &Path, rev: Option<&str>) -> Result<(String, String), String> {
    if let Some(r) = rev {
        let sha = git(root, &["rev-parse", "--verify", &format!("{r}^{{commit}}")])?;
        return Ok((sha.trim().to_string(), r.to_string()));
    }
    for m in ["main", "origin/main"] {
        if let Ok(sha) = git(root, &["merge-base", "HEAD", m]) {
            return Ok((sha.trim().to_string(), format!("merge-base with {m}")));
        }
    }
    Err("no merge-base with main or origin/main; pass --base REV".into())
}

/// Committed since `base`, staged, unstaged and untracked files (repo-relative, sorted).
fn changed_files(root: &Path, base: &str) -> Result<Vec<String>, String> {
    let diff = git(root, &["diff", "--name-only", "--no-renames", base])?;
    let untracked = git(root, &["ls-files", "--others", "--exclude-standard"])?;
    let set: BTreeSet<String> = diff.lines().chain(untracked.lines()).map(str::trim).filter(|l| !l.is_empty()).map(str::to_string).collect();
    Ok(set.into_iter().collect())
}

fn short_list(names: &BTreeSet<String>) -> String {
    if names.is_empty() { "none".into() } else { names.iter().map(|n| crate::short(n)).collect::<Vec<_>>().join(", ") }
}

fn first_few(files: &[String]) -> String {
    let mut s = files.iter().take(3).cloned().collect::<Vec<_>>().join(", ");
    if files.len() > 3 {
        s.push_str(&format!(", … (+{})", files.len() - 3));
    }
    s
}

/// One CI step: label and the cargo invocation (None = an in-process check).
struct Step {
    label: String,
    cmd: Option<Command>,
    check: Option<fn() -> Result<(), String>>,
}

fn cargo(args: &[&str], pkgs: &[&String], tail: &[&str]) -> Command {
    let mut c = Command::new(env!("CARGO"));
    c.args(args);
    for p in pkgs {
        c.args(["-p", p.as_str()]);
    }
    c.args(tail);
    c
}

fn steps(plan: &Plan, pkgs: &[Pkg], nextest: bool) -> Vec<Step> {
    let mut v = vec![Step { label: "fmt".into(), cmd: Some(cargo(&["fmt", "--check"], &[], &[])), check: None }];
    let aff: Vec<&String> = plan.affected.iter().collect();
    if !aff.is_empty() {
        let n = aff.len();
        v.push(Step {
            label: format!("clippy ({n})"),
            cmd: Some(cargo(&["clippy", "--all-targets", "--release"], &aff, &["--", "-D", "warnings"])),
            check: None,
        });
        if nextest {
            v.push(Step {
                label: format!("nextest ({n})"),
                cmd: Some(cargo(&["nextest", "run", "--release", "--no-tests=pass", "--no-fail-fast"], &aff, &[])),
                check: None,
            });
            // Same package set as nextest (bin-only packages are skipped by cargo), so features
            // unify the same way and the dependency builds are reused instead of rebuilt.
            let libs = aff.iter().filter(|a| pkgs.iter().any(|p| p.name == ***a && p.has_lib)).count();
            if libs > 0 {
                v.push(Step {
                    label: format!("doctests ({libs})"),
                    cmd: Some(cargo(&["test", "--release", "--doc", "--no-fail-fast"], &aff, &[])),
                    check: None,
                });
            }
        } else {
            v.push(Step { label: format!("test ({n})"), cmd: Some(cargo(&["test", "--release", "--no-fail-fast"], &aff, &[])), check: None });
        }
    }
    v.push(Step { label: "layers".into(), cmd: None, check: Some(crate::layers) });
    if plan.assets {
        v.push(Step { label: "assets".into(), cmd: None, check: Some(crate::assets) });
    }
    let wasm: Vec<&String> = aff.iter().copied().filter(|a| crate::layer_of(a).is_some_and(|l| l <= 4) || crate::WEB_CRATES.contains(&a.as_str())).collect();
    if !wasm.is_empty() {
        v.push(Step { label: format!("wasm ({})", wasm.len()), cmd: Some(cargo(&["check", "--target", "wasm32-unknown-unknown"], &wasm, &[])), check: None });
    }
    v
}

fn fmt_dur(d: Duration) -> String {
    let s = d.as_secs();
    if s >= 60 { format!("{}m{:02}s", s / 60, s % 60) } else { format!("{:.1}s", d.as_secs_f64()) }
}

fn summary(times: &[(String, Duration, bool)]) {
    let total: Duration = times.iter().map(|t| t.1).sum();
    eprintln!("\nci --affected timing:");
    for (label, d, ok) in times {
        eprintln!("  {:<16} {:>8}  {}", label, fmt_dur(*d), if *ok { "ok" } else { "FAILED" });
    }
    eprintln!("  {:<16} {:>8}", "total", fmt_dur(total));
}

pub fn run(root: &Path, args: &[String]) -> Result<(), String> {
    let dry = args.iter().any(|a| a == "--dry-run");
    let rev = match args.iter().position(|a| a == "--base") {
        Some(i) => Some(args.get(i + 1).map(String::as_str).ok_or("--base needs a revision")?),
        None => None,
    };
    let (sha, why) = base(root, rev)?;
    let files = changed_files(root, &sha)?;
    let md = crate::metadata()?;
    let pkgs = packages(root, &md);
    let plan = plan(&files, &pkgs);
    let short_sha = sha.get(..9).unwrap_or(&sha);
    println!("ci --affected: base {short_sha} ({why}), {} changed file(s)", files.len());
    if let Some(reason) = &plan.full {
        println!("  full CI: {reason}");
        if dry {
            return Ok(());
        }
        let t = Instant::now();
        let r = crate::ci();
        summary(&[("full ci".into(), t.elapsed(), r.is_ok())]);
        return r;
    }
    println!("  changed: {}", short_list(&plan.direct));
    println!("  affected: {} ({} of {} crates)", short_list(&plan.affected), plan.affected.len(), pkgs.len());
    if !plan.unmapped.is_empty() {
        println!("  read by no crate: {}", first_few(&plan.unmapped));
    }
    let nextest = Command::new(env!("CARGO")).args(["nextest", "--version"]).output().is_ok_and(|o| o.status.success());
    let steps = steps(&plan, &pkgs, nextest);
    let labels: Vec<&str> = steps.iter().map(|s| s.label.as_str()).collect();
    println!("  steps: {}{}", labels.join(", "), if plan.assets { "" } else { " (assets skipped: no asset changes)" });
    if dry {
        return Ok(());
    }
    let mut times = vec![];
    for mut s in steps {
        let t = Instant::now();
        let r = match (s.cmd.as_mut(), s.check) {
            (Some(c), _) => crate::run(c),
            (None, Some(f)) => f(),
            (None, None) => Ok(()),
        };
        times.push((s.label, t.elapsed(), r.is_ok()));
        if let Err(e) = r {
            summary(&times);
            return Err(e);
        }
    }
    summary(&times);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pkg(name: &str, dir: &str, deps: &[(&str, bool)], inputs: &[&str]) -> Pkg {
        Pkg {
            name: name.into(),
            dir: dir.into(),
            deps: deps.iter().map(|(d, dev)| (d.to_string(), *dev)).collect(),
            has_lib: true,
            inputs: inputs.iter().map(|s| s.to_string()).collect(),
        }
    }

    /// time ← render ← engine ← ui ← app; host has engine as a dev-dependency; text reads fonts.
    fn ws() -> Vec<Pkg> {
        vec![
            pkg("effectcraft-time", "crates/time", &[], &[]),
            pkg("effectcraft-text", "crates/text", &[], &["assets/fonts"]),
            pkg("effectcraft-render", "crates/render", &[("effectcraft-time", false), ("effectcraft-text", false)], &[]),
            pkg("effectcraft-engine", "crates/engine", &[("effectcraft-render", false)], &["docs/preferences.md"]),
            pkg("effectcraft-host", "crates/host", &[("effectcraft-time", false), ("effectcraft-engine", true)], &["crates/svg/tests/fixtures"]),
            pkg("effectcraft-ui-egui", "crates/ui-egui", &[("effectcraft-engine", false)], &[]),
            pkg("effectcraft", "apps/effectcraft", &[("effectcraft-ui-egui", false)], &[]),
            pkg("effectcraft-cli", "apps/effectcraft-cli", &[("effectcraft", false)], &[]),
            pkg("effectcraft-svg", "crates/svg", &[], &[]),
        ]
    }

    fn set(v: &[&str]) -> BTreeSet<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    fn files(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn file_maps_to_innermost_package() {
        let mut p = ws();
        p.push(pkg("nested", "crates/time/nested", &[], &[]));
        assert_eq!(owner("crates/time/src/lib.rs", &p).map(|p| p.name.as_str()), Some("effectcraft-time"));
        assert_eq!(owner("crates/time/nested/src/lib.rs", &p).map(|p| p.name.as_str()), Some("nested"));
        // a prefix of the dir name is not a match
        assert_eq!(owner("crates/timeline/src/lib.rs", &p).map(|p| p.name.as_str()), None);
        assert_eq!(owner("apps/effectcraft-cli/src/main.rs", &p).map(|p| p.name.as_str()), Some("effectcraft-cli"));
        assert_eq!(owner("apps/effectcraft/src/main.rs", &p).map(|p| p.name.as_str()), Some("effectcraft"));
        assert!(owner("docs/testing.md", &p).is_none());
    }

    #[test]
    fn reverse_deps_follow_normal_edges_and_one_dev_hop() {
        let p = ws();
        // time: render, engine, ui, app, cli via normal edges; host directly; host is not
        // a dependency of anything.
        let a = expand(&set(&["effectcraft-time"]), &p);
        assert_eq!(
            a,
            set(&["effectcraft-time", "effectcraft-render", "effectcraft-engine", "effectcraft-host", "effectcraft-ui-egui", "effectcraft", "effectcraft-cli"])
        );
        // ui-egui: only its dependents
        assert_eq!(expand(&set(&["effectcraft-ui-egui"]), &p), set(&["effectcraft-ui-egui", "effectcraft", "effectcraft-cli"]));
        // engine: host only through its dev-dependency, and nothing beyond host
        let e = expand(&set(&["effectcraft-engine"]), &p);
        assert!(e.contains("effectcraft-host"));
        assert!(!e.contains("effectcraft-time") && !e.contains("effectcraft-render"));
        assert_eq!(expand(&set(&["effectcraft-cli"]), &p), set(&["effectcraft-cli"]));
        assert!(expand(&BTreeSet::new(), &p).is_empty());
    }

    #[test]
    fn workspace_inputs_fall_back_to_full_ci() {
        let p = ws();
        for f in ["Cargo.toml", "Cargo.lock", ".cargo/config.toml", "xtask/src/main.rs", "rust-toolchain.toml", "clippy.toml"] {
            let pl = plan(&files(&["crates/time/src/lib.rs", f]), &p);
            assert!(pl.full.is_some(), "{f}");
        }
        // a crate manifest is not workspace-wide
        assert_eq!(plan(&files(&["crates/time/Cargo.toml"]), &p).full, None);
        assert_eq!(full_ci_reason("docs/xtask.md"), None);
    }

    #[test]
    fn docs_only_changes_affect_nothing() {
        let pl = plan(&files(&["docs/gaps.md", "README.md", ".github/workflows/release.yml"]), &ws());
        assert_eq!(pl.full, None);
        assert!(pl.affected.is_empty());
        assert!(!pl.assets);
        assert_eq!(pl.unmapped.len(), 3);
    }

    #[test]
    fn files_read_from_outside_map_to_their_readers() {
        let p = ws();
        // docs/preferences.md is read by an engine test
        let pl = plan(&files(&["docs/preferences.md"]), &p);
        assert_eq!(pl.direct, set(&["effectcraft-engine"]));
        assert!(pl.unmapped.is_empty());
        // a font: read by text (directory input), and an asset
        let pl = plan(&files(&["assets/fonts/Inter-Regular.ttf"]), &p);
        assert_eq!(pl.direct, set(&["effectcraft-text"]));
        assert!(pl.assets);
        assert!(pl.affected.contains("effectcraft-cli"));
        // svg fixtures belong to svg and are read by host
        let pl = plan(&files(&["crates/svg/tests/fixtures/basic-shapes.svg"]), &p);
        assert_eq!(pl.direct, set(&["effectcraft-svg", "effectcraft-host"]));
    }

    #[test]
    fn asset_rules() {
        assert!(is_asset_related("ATTRIBUTION.md"));
        assert!(is_asset_related("crates/engine/scripts/Rename Layers.jsx.attribution"));
        assert!(is_asset_related("docs/brand/artcraft-mark.png"));
        assert!(!is_asset_related("docs/testing.md"));
        assert!(!is_asset_related("crates/time/src/lib.rs"));
    }

    #[test]
    fn rel_path_extraction() {
        assert_eq!(rel_paths(r#"static F: &[u8] = include_bytes!("../../../assets/fonts/Inter-Regular.ttf");"#), ["../../../assets/fonts/Inter-Regular.ttf"]);
        assert_eq!(rel_paths(r#"let p = concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/effects.md");"#), ["../../docs/effects.md"]);
        assert_eq!(rel_paths(r#"format!("{}/../model/tests/fixtures/{n}", env!("CARGO_MANIFEST_DIR"))"#), ["../model/tests/fixtures"]);
        assert_eq!(rel_paths(r#"let a = "x"; let b = "../y/z{n}.png";"#), ["../y"]);
        assert!(rel_paths(r#"let s = "no parent here";"#).is_empty());
    }

    #[test]
    fn rel_path_joining() {
        assert_eq!(join_rel("crates/text/src", "../../../assets/fonts/A.ttf").as_deref(), Some("assets/fonts/A.ttf"));
        assert_eq!(join_rel("crates/host", "../model/./tests").as_deref(), Some("crates/model/tests"));
        assert_eq!(join_rel("crates", "../../outside"), None);
    }

    #[test]
    fn inputs_try_file_and_manifest_dirs() {
        let existing = set(&["assets/fonts/Inter-Regular.ttf", "docs/effects.md", "crates/model/tests/fixtures", "crates/svg/tests/fixtures/a.svg"]);
        let exists = |p: &str| existing.contains(p);
        let src = r#"
static F: &[u8] = include_bytes!("../../../assets/fonts/Inter-Regular.ttf");
let p = concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/effects.md");
let fx = format!("{}/../model/tests/fixtures/{n}", env!("CARGO_MANIFEST_DIR"));
let own = include_bytes!("../tests/fixtures/own.svg");
let out = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/test-out");
// let commented = include_bytes!("../../svg/tests/fixtures/a.svg");
"#;
        let got = inputs_of("crates/host/src/lib.rs", "crates/host", src, &exists);
        assert_eq!(got, set(&["assets/fonts/Inter-Regular.ttf", "docs/effects.md", "crates/model/tests/fixtures"]));
        // integration tests in tests/ resolve include_bytes! against tests/
        let got = inputs_of("crates/host/tests/v.rs", "crates/host", r#"include_bytes!("../../svg/tests/fixtures/a.svg")"#, &exists);
        assert_eq!(got, set(&["crates/svg/tests/fixtures/a.svg"]));
    }

    fn labels(s: &[Step]) -> Vec<String> {
        s.iter().map(|s| s.label.clone()).collect()
    }

    #[test]
    fn steps_follow_the_plan() {
        let mut p = ws();
        if let Some(cli) = p.iter_mut().find(|p| p.name == "effectcraft-cli") {
            cli.has_lib = false;
        }
        // docs only: cheap checks
        assert_eq!(labels(&steps(&plan(&files(&["docs/gaps.md"]), &p), &p, true)), ["fmt", "layers"]);
        // ui-egui (L5, a web crate) and its two app dependents; cli has no doctests
        let s = steps(&plan(&files(&["crates/ui-egui/src/dock.rs"]), &p), &p, true);
        assert_eq!(labels(&s), ["fmt", "clippy (3)", "nextest (3)", "doctests (2)", "layers", "wasm (1)"]);
        let args = |i: usize| s.get(i).and_then(|s| s.cmd.as_ref()).map(|c| c.get_args().map(|a| a.to_string_lossy().into_owned()).collect::<Vec<_>>());
        let clippy = args(1).unwrap_or_default();
        assert!(clippy.windows(2).any(|w| w == ["-p", "effectcraft-ui-egui"]));
        assert!(clippy.ends_with(&["--".into(), "-D".into(), "warnings".into()]));
        assert!(args(5).unwrap_or_default().windows(2).any(|w| w == ["-p", "effectcraft-ui-egui"]));
        // without nextest: one cargo test step; an asset change adds the assets check
        let s = steps(&plan(&files(&["assets/fonts/A.ttf"]), &p), &p, false);
        assert!(labels(&s).contains(&"test (7)".to_string()) && labels(&s).contains(&"assets".to_string()));
    }
}
