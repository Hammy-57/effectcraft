//! `cargo xtask agents <status|claim|release|gc>`: coordination for parallel agents.
//!
//! - `status`: every worktree (branch, age, commits ahead of main, dirty files, verdict), every
//!   `target/agent-*` dir (size, age, owner or ORPHAN) and the active claims. Also written to
//!   `plan/STATUS.md` in the main checkout.
//! - `claim <slug> --task "…" [--paths a,b]`: writes `plan/claims/<slug>.toml` with a target dir and
//!   a free control port, warns about overlapping paths and prints the env lines to use.
//! - `release <slug>`: deletes the claim.
//! - `gc [--yes]`: lists merged + clean worktrees and orphan target dirs (dry run); `--yes` removes
//!   only those. Never touches dirty, locked, unmerged or claimed ones, or the main checkout.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

const USAGE: &str = "usage: cargo xtask agents <status | claim SLUG --task TEXT [--paths a,b] | release SLUG | gc [--yes]>";
/// Control ports handed out by `claim` start here (9877 is the owner's app).
const FIRST_PORT: u16 = 9880;
/// Walk depth limit for sizing target dirs.
const MAX_DEPTH: u32 = 24;
/// An unowned target dir untouched for this long counts as an orphan.
const UNKNOWN_ORPHAN_SECS: u64 = 24 * 3600;
/// A target dir written this recently is treated as in use whatever its dep-info says.
const RECENT_SECS: u64 = 3600;

pub fn run(args: &[String]) -> Result<(), String> {
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    match a.as_slice() {
        ["status"] | [] => status(),
        ["claim", slug, rest @ ..] => claim(slug, rest),
        ["release", slug] => release(slug),
        ["gc"] => gc(false),
        ["gc", "--yes"] => gc(true),
        _ => Err(USAGE.into()),
    }
}

// ------------------------------------------------------------------------------------- model

struct Worktree {
    path: PathBuf,
    branch: String,
    locked: bool,
    missing: bool,
    age: Option<u64>,
    ahead: u64,
    dirty: usize,
}

impl Worktree {
    fn verdict(&self, main: &Path) -> &'static str {
        if self.path == main {
            "main"
        } else if self.missing {
            "missing (git worktree prune)"
        } else if self.ahead > 0 {
            "unmerged work"
        } else if self.dirty > 0 {
            "merged but dirty"
        } else {
            "safe to remove"
        }
    }
}

struct TargetDir {
    path: PathBuf,
    bytes: u64,
    age: Option<u64>,
    /// Checkouts that built into it (from cargo dep-info files).
    roots: BTreeSet<PathBuf>,
}

#[derive(Default)]
struct Claim {
    slug: String,
    task: String,
    paths: Vec<String>,
    branch: String,
    worktree: String,
    target_dir: String,
    port: u16,
    claimed: String,
    claimed_unix: u64,
}

struct State {
    main: PathBuf,
    worktrees: Vec<Worktree>,
    targets: Vec<TargetDir>,
    claims: Vec<Claim>,
}

impl State {
    fn load() -> Result<State, String> {
        let main = main_checkout()?;
        let worktrees = worktrees(&main)?;
        let targets = target_dirs(&main);
        let claims = claims(&main)?;
        Ok(State { main, worktrees, targets, claims })
    }

    fn claimed_worktree(&self, p: &Path) -> Option<&Claim> {
        self.claims.iter().find(|c| canon(Path::new(&c.worktree)) == p)
    }

    fn claimed_target(&self, p: &Path) -> Option<&Claim> {
        self.claims.iter().find(|c| canon(Path::new(&c.target_dir)) == p)
    }

    /// Worktrees `gc` may remove.
    fn removable_worktrees(&self) -> Vec<&Worktree> {
        let here = git(&["rev-parse", "--show-toplevel"], None).map(|p| canon(Path::new(&p)));
        self.worktrees
            .iter()
            .filter(|w| w.verdict(&self.main) == "safe to remove" && !w.locked)
            .filter(|w| self.claimed_worktree(&w.path).is_none() && here.as_deref() != Some(w.path.as_path()))
            .collect()
    }

    /// Who uses a target dir: a claim, a live worktree, or nobody (`None` = orphan).
    fn owner(&self, t: &TargetDir, gone: &[&Worktree]) -> Option<String> {
        if let Some(c) = self.claimed_target(&t.path) {
            return Some(format!("claim {}", c.slug));
        }
        let live = |r: &PathBuf| self.worktrees.iter().any(|w| &w.path == r && !w.missing && !gone.iter().any(|g| &g.path == r));
        if let Some(r) = t.roots.iter().find(|r| live(r)) {
            return Some(short(r, &self.main));
        }
        if std::env::var_os("CARGO_TARGET_DIR").is_some_and(|d| canon(Path::new(&d)) == t.path) {
            return Some("this shell".into());
        }
        if t.age.is_none_or(|a| a < RECENT_SECS) {
            return Some("in use? (written <1h ago)".into());
        }
        if t.roots.is_empty() && t.age.is_none_or(|a| a < UNKNOWN_ORPHAN_SECS) {
            return Some("unknown (recent)".into());
        }
        None
    }
}

// ------------------------------------------------------------------------------------- status

fn status() -> Result<(), String> {
    let s = State::load()?;
    let text = report(&s);
    print!("{text}");
    let dir = s.main.join("plan");
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let md = format!("# Agent status\n\nGenerated by `cargo xtask agents status` at {}; do not edit (it is overwritten).\n\n```text\n{text}```\n", utc(now()));
    let file = dir.join("STATUS.md");
    std::fs::write(&file, md).map_err(|e| format!("{}: {e}", file.display()))?;
    println!("\n(written to {})", file.display());
    Ok(())
}

fn report(s: &State) -> String {
    let mut o = String::new();
    let main = &s.main;
    let _ = writeln!(o, "Worktrees (main checkout {}):", main.display());
    let _ = writeln!(o, "  {:44} {:34} {:>5} {:>5} {:>5} {:>6}  verdict", "worktree", "branch", "age", "ahead", "dirty", "merged");
    for w in &s.worktrees {
        let mut tags = String::new();
        if w.locked {
            tags.push_str(" (locked)");
        }
        if let Some(c) = s.claimed_worktree(&w.path) {
            let _ = write!(tags, " (claim {})", c.slug);
        }
        let _ = writeln!(
            o,
            "  {:44} {:34} {:>5} {:>5} {:>5} {:>6}  {}{tags}",
            clip(&short(&w.path, main), 44),
            clip(&w.branch, 34),
            age(w.age),
            w.ahead,
            w.dirty,
            if w.ahead == 0 { "yes" } else { "no" },
            w.verdict(main)
        );
    }
    let _ = writeln!(o, "\nTarget dirs (target/agent-*):");
    let (mut total, mut orphan) = (0u64, 0u64);
    for t in &s.targets {
        total = total.saturating_add(t.bytes);
        let owner = s.owner(t, &[]);
        if owner.is_none() {
            orphan = orphan.saturating_add(t.bytes);
        }
        let name = t.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let _ = writeln!(o, "  {:28} {:>8} {:>5}  {}", clip(&name, 28), size(t.bytes), age(t.age), owner.as_deref().unwrap_or("ORPHAN"));
    }
    let _ = writeln!(o, "  total {}, orphan {}", size(total), size(orphan));
    let _ = writeln!(o, "\nClaims (plan/claims/):");
    if s.claims.is_empty() {
        let _ = writeln!(o, "  none");
    }
    for c in &s.claims {
        let gone = if Path::new(&c.worktree).exists() { "" } else { " (worktree gone: release it)" };
        let _ = writeln!(
            o,
            "  {:20} port {} {:>5}  {}  {}{gone}\n  {:20} paths: {}",
            clip(&c.slug, 20),
            c.port,
            age(now().checked_sub(c.claimed_unix)),
            c.branch,
            c.task,
            "",
            if c.paths.is_empty() { "-".into() } else { c.paths.join(", ") }
        );
    }
    o
}

// ------------------------------------------------------------------------------------- claims

fn claim(slug: &str, rest: &[&str]) -> Result<(), String> {
    check_slug(slug)?;
    let (mut task, mut paths) = (None, Vec::new());
    let mut it = rest.iter();
    while let Some(a) = it.next() {
        match *a {
            "--task" => task = it.next().map(|t| t.to_string()),
            "--paths" => {
                paths =
                    it.next().map(|p| p.split(',').map(|s| s.trim().trim_end_matches('/').to_string()).filter(|s| !s.is_empty()).collect()).unwrap_or_default()
            }
            _ => return Err(USAGE.into()),
        }
    }
    let task = task.ok_or("claim needs --task \"what you are doing\"")?;
    let main = main_checkout()?;
    let others: Vec<Claim> = claims(&main)?.into_iter().filter(|c| c.slug != slug).collect();
    for c in &others {
        for p in &paths {
            if let Some(q) = c.paths.iter().find(|q| overlaps(p, q)) {
                eprintln!("warning: {p} overlaps {q} claimed by {} ({}, {})", c.slug, c.branch, c.task);
            }
        }
    }
    let used: BTreeSet<u16> = others.iter().map(|c| c.port).collect();
    let port = (FIRST_PORT..u16::MAX).find(|p| !used.contains(p) && std::net::TcpListener::bind(("127.0.0.1", *p)).is_ok()).ok_or("no free control port")?;
    let worktree = git(&["rev-parse", "--show-toplevel"], None).ok_or("not inside a git checkout")?;
    let branch = git(&["branch", "--show-current"], None).unwrap_or_default();
    let target = main.join("target").join(format!("agent-{slug}"));
    let t = now();
    let c =
        Claim { slug: slug.into(), task, paths, branch, worktree, target_dir: target.to_string_lossy().into_owned(), port, claimed: utc(t), claimed_unix: t };
    let dir = main.join("plan").join("claims");
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let file = dir.join(format!("{slug}.toml"));
    std::fs::write(&file, write_claim(&c)).map_err(|e| format!("{}: {e}", file.display()))?;
    println!("claimed {slug} ({})", file.display());
    println!("export CARGO_TARGET_DIR={} EFFECTCRAFT_CONTROL_PORT={port} CARGO_BUILD_JOBS=4", c.target_dir);
    println!("# when done: cargo xtask agents release {slug}");
    Ok(())
}

fn release(slug: &str) -> Result<(), String> {
    check_slug(slug)?;
    let file = main_checkout()?.join("plan").join("claims").join(format!("{slug}.toml"));
    std::fs::remove_file(&file).map_err(|e| format!("{}: {e}", file.display()))?;
    println!("released {slug}");
    Ok(())
}

fn check_slug(slug: &str) -> Result<(), String> {
    let ok = !slug.is_empty() && slug.len() <= 64 && slug.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if ok { Ok(()) } else { Err(format!("bad slug {slug:?}: use letters, digits, - and _")) }
}

/// One path is the other or contains it (component-wise).
fn overlaps(a: &str, b: &str) -> bool {
    let within = |x: &str, y: &str| x == y || x.strip_prefix(y).is_some_and(|r| r.starts_with('/'));
    within(a, b) || within(b, a)
}

fn quote(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n"))
}

/// The quoted strings in a TOML value (`"a"` or `["a", "b"]`), unescaped.
fn strings(v: &str) -> Vec<String> {
    let (mut out, mut cur, mut inside, mut esc) = (Vec::new(), String::new(), false, false);
    for c in v.chars() {
        if esc {
            cur.push(if c == 'n' { '\n' } else { c });
            esc = false;
        } else if inside && c == '\\' {
            esc = true;
        } else if c == '"' {
            if inside {
                out.push(std::mem::take(&mut cur));
            }
            inside = !inside;
        } else if inside {
            cur.push(c);
        }
    }
    out
}

fn unquote(v: &str) -> String {
    strings(v).into_iter().next().unwrap_or_default()
}

fn write_claim(c: &Claim) -> String {
    let paths: Vec<String> = c.paths.iter().map(|p| quote(p)).collect();
    format!(
        "# Written by `cargo xtask agents claim`; remove with `cargo xtask agents release {}`.\ntask = {}\npaths = [{}]\nbranch = {}\nworktree = {}\ntarget_dir = {}\nport = {}\nclaimed = {}\nclaimed_unix = {}\n",
        c.slug,
        quote(&c.task),
        paths.join(", "),
        quote(&c.branch),
        quote(&c.worktree),
        quote(&c.target_dir),
        c.port,
        quote(&c.claimed),
        c.claimed_unix
    )
}

fn parse_claim(slug: &str, text: &str) -> Claim {
    let mut c = Claim { slug: slug.into(), ..Default::default() };
    for line in text.lines() {
        let Some((k, v)) = line.split_once('=') else { continue };
        let v = v.trim();
        match k.trim() {
            "task" => c.task = unquote(v),
            "paths" => c.paths = strings(v),
            "branch" => c.branch = unquote(v),
            "worktree" => c.worktree = unquote(v),
            "target_dir" => c.target_dir = unquote(v),
            "port" => c.port = v.parse().unwrap_or(0),
            "claimed" => c.claimed = unquote(v),
            "claimed_unix" => c.claimed_unix = v.parse().unwrap_or(0),
            _ => {}
        }
    }
    c
}

fn claims(main: &Path) -> Result<Vec<Claim>, String> {
    let dir = main.join("plan").join("claims");
    let Ok(rd) = std::fs::read_dir(&dir) else { return Ok(Vec::new()) };
    let mut out = Vec::new();
    for e in rd.flatten() {
        let p = e.path();
        let (Some(stem), Some("toml")) = (p.file_stem().and_then(|s| s.to_str()), p.extension().and_then(|s| s.to_str())) else { continue };
        let text = std::fs::read_to_string(&p).map_err(|e| format!("{}: {e}", p.display()))?;
        out.push(parse_claim(stem, &text));
    }
    out.sort_by(|a, b| a.slug.cmp(&b.slug));
    Ok(out)
}

// ------------------------------------------------------------------------------------- gc

fn gc(yes: bool) -> Result<(), String> {
    let s = State::load()?;
    let wts = s.removable_worktrees();
    let tgts: Vec<&TargetDir> = s.targets.iter().filter(|t| s.owner(t, &wts).is_none()).collect();
    let mut total = 0u64;
    println!("Merged + clean worktrees:");
    let mut wt_sizes = Vec::new();
    for w in &wts {
        let b = scan(&w.path, &s.main).bytes;
        total = total.saturating_add(b);
        wt_sizes.push(b);
        println!("  {:8}  {}  ({})", size(b), short(&w.path, &s.main), w.branch);
    }
    println!("Orphan target dirs (incl. those of the worktrees above):");
    for t in &tgts {
        total = total.saturating_add(t.bytes);
        println!("  {:8}  {}", size(t.bytes), short(&t.path, &s.main));
    }
    println!("Total {}", size(total));
    if !yes {
        println!("Dry run: `cargo xtask agents gc --yes` removes exactly these.");
        return Ok(());
    }
    let mut failed = Vec::new();
    for w in &wts {
        let p = w.path.to_string_lossy();
        if git(&["worktree", "remove", &p], Some(&s.main)).is_none() {
            failed.push(format!("git worktree remove {p}"));
        }
    }
    for t in &tgts {
        if let Err(e) = std::fs::remove_dir_all(&t.path) {
            failed.push(format!("{}: {e}", t.path.display()));
        }
    }
    if failed.is_empty() {
        println!("removed");
        Ok(())
    } else {
        Err(format!("some removals failed:\n  {}", failed.join("\n  ")))
    }
}

// ------------------------------------------------------------------------------------- git / fs

fn git(args: &[&str], cwd: Option<&Path>) -> Option<String> {
    let mut cmd = Command::new("git");
    cmd.args(args);
    if let Some(d) = cwd {
        cmd.current_dir(d);
    }
    let out = cmd.output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn canon(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

/// The main checkout: the parent of the shared `.git` dir.
fn main_checkout() -> Result<PathBuf, String> {
    let common = git(&["rev-parse", "--path-format=absolute", "--git-common-dir"], None).ok_or("not inside a git checkout")?;
    let common = canon(Path::new(&common));
    common.parent().map(Path::to_path_buf).ok_or_else(|| format!("{} has no parent", common.display()))
}

fn worktrees(main: &Path) -> Result<Vec<Worktree>, String> {
    let list = git(&["worktree", "list", "--porcelain"], Some(main)).ok_or("git worktree list failed")?;
    let mut out = Vec::new();
    for block in list.split("\n\n") {
        let (mut path, mut head, mut branch, mut locked, mut prunable) = (None, String::new(), "(detached)".to_string(), false, false);
        for line in block.lines() {
            let (k, v) = line.split_once(' ').unwrap_or((line, ""));
            match k {
                "worktree" => path = Some(canon(Path::new(v))),
                "HEAD" => head = v.to_string(),
                "branch" => branch = v.trim_start_matches("refs/heads/").to_string(),
                "locked" => locked = true,
                "prunable" => prunable = true,
                _ => {}
            }
        }
        let Some(path) = path else { continue };
        let missing = prunable || !path.exists();
        let ts = git(&["log", "-1", "--format=%ct", &head], Some(main)).and_then(|t| t.parse::<u64>().ok());
        let ahead = git(&["rev-list", "--count", &format!("main..{head}")], Some(main)).and_then(|n| n.parse().ok()).unwrap_or(0);
        let dirty =
            if missing { 0 } else { git(&["status", "--porcelain"], Some(&path)).map(|s| s.lines().filter(|l| !l.trim().is_empty()).count()).unwrap_or(0) };
        out.push(Worktree { path, branch, locked, missing, age: ts.and_then(|t| now().checked_sub(t)), ahead, dirty });
    }
    Ok(out)
}

fn target_dirs(main: &Path) -> Vec<TargetDir> {
    let Ok(rd) = std::fs::read_dir(main.join("target")) else { return Vec::new() };
    let mut dirs: Vec<PathBuf> =
        rd.flatten().filter(|e| e.file_name().to_string_lossy().starts_with("agent-") && e.file_type().is_ok_and(|t| t.is_dir())).map(|e| e.path()).collect();
    dirs.sort();
    // One thread per dir: sizing hundreds of GB is I/O bound.
    std::thread::scope(|sc| {
        let handles: Vec<_> = dirs.iter().map(|d| (d, sc.spawn(|| scan(d, main)))).collect();
        handles.into_iter().filter_map(|(d, h)| h.join().ok().map(|s| TargetDir { path: canon(d), bytes: s.bytes, age: s.age, roots: s.roots })).collect()
    })
}

#[derive(Default)]
struct Scan {
    bytes: u64,
    age: Option<u64>,
    roots: BTreeSet<PathBuf>,
}

/// Size (apparent bytes), age of the newest file, and the checkouts named in workspace dep-info files.
/// Skips symlinks; depth-limited.
fn scan(dir: &Path, main: &Path) -> Scan {
    let mut s = Scan::default();
    let mut newest: Option<SystemTime> = None;
    let mut reads = 0;
    let mut stack = vec![(dir.to_path_buf(), 0u32)];
    while let Some((d, depth)) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_symlink() {
                continue;
            }
            if ft.is_dir() {
                if depth < MAX_DEPTH {
                    stack.push((e.path(), depth + 1));
                }
                continue;
            }
            let Ok(md) = e.metadata() else { continue };
            s.bytes = s.bytes.saturating_add(md.len());
            newest = newest.max(md.modified().ok());
            let name = e.file_name();
            let name = name.to_string_lossy();
            // Uplifted `*.d` files and workspace build-script `output` files carry absolute source paths.
            let dep_info = name.ends_with(".d") && (name.contains("effectcraft") || name.starts_with("xtask"));
            let build_out = name == "output" && d.file_name().is_some_and(|p| p.to_string_lossy().starts_with("effectcraft"));
            if reads < 200 && (dep_info || build_out) {
                reads += 1;
                dep_info_roots(&e.path(), main, &mut s.roots);
            }
        }
    }
    s.age = newest.and_then(|t| t.elapsed().ok()).map(|d| d.as_secs());
    s
}

/// Checkout roots in a cargo dep-info / build-script output file: `<root>/{crates,apps,xtask}/…` paths under the main
/// checkout or in a sibling `effectcraft*` dir.
fn dep_info_roots(file: &Path, main: &Path, out: &mut BTreeSet<PathBuf>) {
    let Some(prefix) = main.parent().map(|p| format!("{}/", p.display())) else { return };
    let mut buf = Vec::new();
    if std::fs::File::open(file).and_then(|f| f.take(4096).read_to_end(&mut buf)).is_err() {
        return;
    }
    let text = String::from_utf8_lossy(&buf);
    for (i, _) in text.match_indices(&prefix) {
        let tok = text.get(i..).and_then(|t| t.split(|c: char| c.is_whitespace() || c == ':').next()).unwrap_or("");
        let Some(cut) = ["/crates/", "/apps/", "/xtask/"].iter().filter_map(|m| tok.find(m)).min() else { continue };
        let root = PathBuf::from(tok.get(..cut).unwrap_or(""));
        let named = root.file_name().is_some_and(|n| n.to_string_lossy().starts_with("effectcraft"));
        if (root.starts_with(main) || named) && !root.starts_with(main.join("target")) {
            out.insert(canon(&root));
        }
    }
}

// ------------------------------------------------------------------------------------- formatting

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// `YYYY-MM-DD HH:MM UTC` (civil-from-days, proleptic Gregorian).
pub(crate) fn utc(t: u64) -> String {
    let (days, secs) = ((t / 86_400) as i64, t % 86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02} UTC", secs / 3600, secs % 3600 / 60)
}

fn age(secs: Option<u64>) -> String {
    match secs {
        None => "-".into(),
        Some(s) if s < 3600 => format!("{}m", s / 60),
        Some(s) if s < 2 * 86_400 => format!("{}h", s / 3600),
        Some(s) => format!("{}d", s / 86_400),
    }
}

fn size(b: u64) -> String {
    let g = b as f64 / (1u64 << 30) as f64;
    if g >= 1.0 { format!("{g:.1}G") } else { format!("{:.0}M", b as f64 / (1u64 << 20) as f64) }
}

fn short(p: &Path, main: &Path) -> String {
    let s = p.to_string_lossy();
    let wt = format!("{}/.claude/worktrees/", main.display());
    if let Some(r) = s.strip_prefix(&wt) {
        return format!(".cw/{r}");
    }
    match main.parent().and_then(|par| p.strip_prefix(par).ok()) {
        Some(r) => format!("../{}", r.display()),
        None => s.into_owned(),
    }
}

fn clip(s: &str, n: usize) -> String {
    if s.chars().count() <= n { s.to_string() } else { s.chars().take(n.saturating_sub(1)).chain(['…']).collect() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claims_round_trip_and_overlap() {
        let c = Claim {
            slug: "x".into(),
            task: "say \"hi\"\\".into(),
            paths: vec!["crates/a".into(), "docs".into()],
            port: 9881,
            claimed_unix: 5,
            ..Default::default()
        };
        let back = parse_claim("x", &write_claim(&c));
        assert_eq!((back.task.as_str(), back.paths.clone(), back.port, back.claimed_unix), (c.task.as_str(), c.paths.clone(), 9881, 5));
        assert!(overlaps("crates/ui-egui", "crates/ui-egui/src/dock.rs"));
        assert!(!overlaps("crates/ui", "crates/ui-egui"));
        assert_eq!(utc(0), "1970-01-01 00:00 UTC");
        assert_eq!(utc(1_791_590_400), "2026-10-10 00:00 UTC");
        assert!(check_slug("../x").is_err());
    }
}
