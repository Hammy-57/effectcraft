//! `mirror <user@host>`: copy the media and fonts a project reads to a cloud render machine
//! (`plan/cloud-render.md` CR7), then check there that nothing is missing.
//!
//! - The manifest is `project.dependencies` of the running app (`--bridge`, default) or of a
//!   project file (`--project`). Media and linked fonts go to the **same absolute paths**
//!   (`/Users/elias/...` on Linux); other font files go to one folder (`--font-dir`), which the
//!   server reads through `EFFECTCRAFT_FONT_DIRS`.
//! - Only what is missing or changed is sent: size + mtime, and for a same-size file with another
//!   mtime the sha256 on both sides (equal: nothing is sent; `--retime` also copies the mtime).
//! - One tar stream over ssh, written here (paths with spaces, no rsync, no AppleDouble `._`
//!   files), with progress; files over 1 GB are listed before anything is sent.
//! - Project files (`.ecproj`, `.aep`) are never written: syncing the project is CR3's.
//! - Afterwards it writes the server's env file (`EFFECTCRAFT_FONT_DIRS`, the Mac's
//!   `EFFECTCRAFT_CJK_LOCALE`) and runs `cloud.preflight` there with the manifest.
//! - `--watch SECS` keeps going: new imports in the app (`cloud.pendingUploads`) are uploaded and
//!   confirmed (`cloud.markUploaded`), so the Mac renders them locally until they are there.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use effectcraft_automation::Backend;
use effectcraft_engine::cloud_assets::{self, BIG_FILE, Manifest};
use serde_json::{Value, json};

use crate::{Args, Failure};

/// The vast.ai template's font folder (the Mac's fonts already live there).
const DEFAULT_FONT_DIR: &str = "/home/user/.local/share/fonts/mac";
const DEFAULT_REMOTE_CLI: &str = "/workspace/effectcraft/target/local/effectcraft-cli";
/// Where the server's shell environment for `serve` lives (`$HOME` there).
const DEFAULT_ENV_FILE: &str = "$HOME/.config/effectcraft/cloud.env";
/// Remote output starts after this line (a login banner may come first).
const MARK: &str = "@@EC-MIRROR@@";

pub fn run(args: &Args, json_out: bool) -> Result<(), Failure> {
    let Some(target) = args.pos.first().cloned() else {
        return Err(Failure::Usage("mirror needs an ssh target: mirror root@HOST --ssh-port P [--identity KEY]".into()));
    };
    let ssh = Ssh::new(target, args.opt("--ssh-port"), args.opt("--identity"));
    let opts = Opts {
        font_dir: args.opt("--font-dir").unwrap_or(DEFAULT_FONT_DIR).trim_end_matches('/').to_string(),
        remote_cli: args.opt("--remote-cli").unwrap_or(DEFAULT_REMOTE_CLI).to_string(),
        env_file: args.opt("--env-file").unwrap_or(DEFAULT_ENV_FILE).to_string(),
        dry_run: args.flag("--dry-run"),
        all: args.flag("--all"),
        verify_hash: args.flag("--verify-hash"),
        retime: args.flag("--retime"),
    };
    let watch = args.num("--watch")?.map(|s| Duration::from_secs_f64(s.clamp(1.0, 3600.0)));
    let mut backend = manifest_backend(args)?;
    let bridged = args.project.is_none();
    say(&format!("📋 reading the project's dependencies{}", if bridged { " from the app" } else { "" }));
    let mut manifest: Manifest = serde_json::from_value(backend.exec("project.dependencies", json!({"hash": "small"}))?)
        .map_err(|e| Failure::Error(format!("project.dependencies: {e}")))?;
    let units = units_of(&manifest, &opts);
    let report = sync(&ssh, &opts, &units, &manifest.host)?;
    with_known_hashes(&mut manifest);
    if bridged && !opts.dry_run {
        mark(&mut backend, &report.confirmed, true)?;
    }
    let preflight = if opts.dry_run { Value::Null } else { remote_preflight(&ssh, &opts, &manifest)? };
    let mut out = json!({
        "uploaded": report.uploaded, "bytes": report.bytes, "upToDate": report.up_to_date, "retimed": report.retimed,
        "missingOnMac": report.missing_on_mac, "nondeterministic": manifest.nondeterministic, "preflight": preflight,
    });
    if json_out {
        crate::emit(&out, true);
    } else {
        say_preflight(&out["preflight"], &manifest);
    }
    let ok = out["preflight"]["ok"].as_bool().unwrap_or(opts.dry_run);
    let Some(every) = watch else {
        return if ok { Ok(()) } else { Err(Failure::Error("the cloud machine is missing files or fonts (above)".into())) };
    };
    if !bridged {
        return Err(Failure::Usage("--watch follows the running app: leave out --project".into()));
    }
    say(&format!("👀 watching the app for new imports every {:.0} s (Ctrl-C stops)", every.as_secs_f64()));
    loop {
        std::thread::sleep(every);
        let pending = match backend.exec("cloud.pendingUploads", json!({})) {
            Ok(p) => p,
            Err(e) => {
                note(&format!("the app didn't answer ({e}); trying again"));
                continue;
            }
        };
        if pending["count"].as_u64().unwrap_or(0) == 0 {
            continue;
        }
        // Fresh hashes and font names for what changed.
        let mut manifest: Manifest = match backend.exec("project.dependencies", json!({"hash": "small"})).map(serde_json::from_value) {
            Ok(Ok(m)) => m,
            _ => continue,
        };
        let wanted: BTreeSet<&str> = pending["pending"].as_array().into_iter().flatten().filter_map(|p| p["path"].as_str()).collect();
        let units: Vec<Unit> = units_of(&manifest, &opts).into_iter().filter(|u| wanted.contains(u.src.as_str())).collect();
        match sync(&ssh, &opts, &units, &manifest.host) {
            Ok(r) => {
                with_known_hashes(&mut manifest);
                mark(&mut backend, &r.confirmed, false)?;
                out = remote_preflight(&ssh, &opts, &manifest)?;
                say_preflight(&out, &manifest);
            }
            Err(e) => note(failure_text(&e)),
        }
    }
}

/// The big files' sha256 this Mac already computed (the mirror hashes a same-size file once):
/// the server's preflight then compares contents instead of refusing an mtime it can't trust.
fn with_known_hashes(m: &mut Manifest) {
    for f in m.files.iter_mut().filter(|f| f.sha256.is_none() && f.exists) {
        f.sha256 = cloud_assets::cached_sha256(&f.path);
    }
}

fn failure_text(f: &Failure) -> &str {
    match f {
        Failure::Usage(s) | Failure::Error(s) => s,
    }
}

fn manifest_backend(args: &Args) -> Result<Backend, Failure> {
    if let Some(p) = &args.project {
        let mut b = Backend::headless(effectcraft_host::session());
        b.exec("file.open", json!({"path": p}))?;
        return Ok(b);
    }
    let addr = args.opt("--bridge").map_or_else(crate::default_control_port, str::to_string);
    Backend::bridge(&addr).map_err(|e| Failure::Error(format!("cannot reach the app on {addr} ({e}); start it, or give --project F.ecproj")))
}

struct Opts {
    font_dir: String,
    remote_cli: String,
    env_file: String,
    dry_run: bool,
    all: bool,
    verify_hash: bool,
    retime: bool,
}

/// One file to have on the server.
#[derive(Clone, Debug)]
struct Unit {
    /// On the Mac.
    src: String,
    /// On the server (the same path, or the font folder).
    dest: String,
    size: u64,
    mtime: i64,
    sha256: Option<String>,
    /// A font for the font folder: present when a file with its hash is there.
    font_folder: bool,
}

fn units_of(m: &Manifest, opts: &Opts) -> Vec<Unit> {
    let mut out: BTreeMap<String, Unit> = BTreeMap::new();
    for f in m.files.iter().filter(|f| f.exists && (f.used || opts.all)) {
        out.insert(
            f.path.clone(),
            Unit { src: f.path.clone(), dest: f.path.clone(), size: f.size, mtime: f.mtime, sha256: f.sha256.clone(), font_folder: false },
        );
    }
    for f in &m.fonts {
        // Faces built into the program are on every machine; linked fonts are files above.
        let (Some(file), false) = (&f.file, f.origin == "bundled" || f.font_file.is_some()) else { continue };
        if out.contains_key(file) {
            continue;
        }
        let Some((size, mtime)) = cloud_assets::stat(file) else { continue };
        let name = std::path::Path::new(file).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "font".into());
        out.insert(
            file.clone(),
            Unit { src: file.clone(), dest: format!("{}/{name}", opts.font_dir), size, mtime, sha256: f.sha256.clone(), font_folder: true },
        );
    }
    out.into_values().collect()
}

#[derive(Default)]
struct Report {
    uploaded: usize,
    bytes: u64,
    up_to_date: usize,
    retimed: usize,
    missing_on_mac: Vec<String>,
    /// Mac path, size, mtime of every unit now on the server.
    confirmed: Vec<Value>,
}

fn sync(ssh: &Ssh, opts: &Opts, units: &[Unit], host: &cloud_assets::HostInfo) -> Result<Report, Failure> {
    let mut report = Report::default();
    let units: Vec<&Unit> = units
        .iter()
        .filter(|u| {
            let project = u.dest.to_ascii_lowercase();
            // Never a project file: the server's copy may be newer (CR3 syncs projects).
            !(project.ends_with(".ecproj") || project.ends_with(".aep"))
        })
        .collect();
    if units.is_empty() {
        say("✅ nothing to mirror");
        return Ok(report);
    }
    say(&format!("🔎 checking {} file(s) on {}", units.len(), ssh.target));
    let media: Vec<&Unit> = units.iter().copied().filter(|u| !u.font_folder).collect();
    let stats = ssh.stat(&media.iter().map(|u| u.dest.as_str()).collect::<Vec<_>>())?;
    let font_hashes = if units.iter().any(|u| u.font_folder) { ssh.font_hashes(&opts.font_dir)? } else { BTreeMap::new() };
    let taken: BTreeSet<String> = font_hashes.values().cloned().collect();
    let mut send: Vec<Unit> = vec![];
    let mut retime: Vec<&Unit> = vec![];
    let mut same_size: Vec<&Unit> = vec![];
    for (u, st) in media.iter().zip(stats) {
        match st {
            Some((size, mtime)) if size == u.size && mtime == u.mtime && !opts.verify_hash => {
                report.up_to_date += 1;
                report.confirmed.push(json!({"path": u.src, "size": u.size, "mtime": u.mtime}));
            }
            Some((size, _)) if size == u.size => same_size.push(u),
            _ => send.push((*u).clone()),
        }
    }
    // Same size, other mtime (copied without keeping it): compare contents before sending GBs.
    if !same_size.is_empty() {
        let remote = ssh.sha256(&same_size.iter().map(|u| u.dest.as_str()).collect::<Vec<_>>())?;
        for (u, theirs) in same_size.into_iter().zip(remote) {
            let mine = match u.sha256.clone().or_else(|| cloud_assets::cached_sha256(&u.src)) {
                Some(h) => Some(h),
                None => {
                    say(&format!("#️⃣  hashing {} ({})", short(&u.src), gb(u.size)));
                    let started = Instant::now();
                    let mut last = Instant::now();
                    cloud_assets::hash_file(&u.src, &mut |done| {
                        if last.elapsed() > Duration::from_secs(1) {
                            last = Instant::now();
                            progress(&format!("   {} / {} · {:.0} MB/s", gb(done), gb(u.size), done as f64 / 1e6 / started.elapsed().as_secs_f64().max(0.001)));
                        }
                    })
                }
            };
            if mine.is_some() && mine.as_deref().map(str::to_ascii_lowercase) == theirs {
                retime.push(u);
            } else {
                send.push(u.clone());
            }
        }
    }
    for u in units.iter().filter(|u| u.font_folder) {
        let have = u.sha256.as_deref().map(str::to_ascii_lowercase).is_some_and(|h| font_hashes.contains_key(&h));
        if have {
            report.up_to_date += 1;
            report.confirmed.push(json!({"path": u.src, "size": u.size, "mtime": u.mtime}));
        } else {
            let mut u = (*u).clone();
            // Another font of the same file name is there: keep both.
            if taken.contains(&u.dest) {
                let tag: String = u.sha256.as_deref().unwrap_or("00000000").chars().take(8).collect();
                let name = u.dest.rsplit('/').next().unwrap_or("font").to_string();
                u.dest = format!("{}/{tag}-{name}", opts.font_dir);
            }
            send.push(u);
        }
    }
    // Same content: the server's preflight compares the hash (once; it remembers). `--retime` also
    // sets the Mac's mtime there, so the next check is size + mtime alone; it is off by default
    // because an app running there may reload footage whose mtime changed.
    if opts.retime && !retime.is_empty() && !opts.dry_run {
        ssh.retime(&retime.iter().map(|u| (u.dest.as_str(), u.mtime)).collect::<Vec<_>>())?;
    }
    for u in &retime {
        report.retimed += 1;
        report.confirmed.push(json!({"path": u.src, "size": u.size, "mtime": u.mtime}));
    }
    // Files change on the Mac between the manifest and now: send what is there now.
    send.retain(|u| match cloud_assets::stat(&u.src) {
        Some(_) => true,
        None => {
            report.missing_on_mac.push(u.src.clone());
            false
        }
    });
    for u in &mut send {
        if let Some((size, mtime)) = cloud_assets::stat(&u.src) {
            (u.size, u.mtime) = (size, mtime);
        }
    }
    let total: u64 = send.iter().map(|u| u.size).fold(0, u64::saturating_add);
    say(&format!("📦 {} up to date, {} same content (other mtime), {} to send ({})", report.up_to_date, report.retimed, send.len(), gb(total)));
    let mut big: Vec<&Unit> = send.iter().filter(|u| u.size > BIG_FILE).collect();
    big.sort_by_key(|u| std::cmp::Reverse(u.size));
    for u in &big {
        say(&format!("   🐘 big file: {} ({})", short(&u.src), gb(u.size)));
    }
    if send.is_empty() {
        return Ok(report);
    }
    if opts.dry_run {
        for u in &send {
            say(&format!("   would send {} → {}", u.src, u.dest));
        }
        return Ok(report);
    }
    let free = ssh.free_bytes()?;
    // Keep 1 GB free on the server (renders write caches and logs).
    if total.saturating_add(BIG_FILE) > free {
        return Err(Failure::Error(format!(
            "the server has {} free and this needs {} (+1 GB headroom): free space there first, nothing was sent",
            gb(free),
            gb(total)
        )));
    }
    // Small files first: fonts and stills are usable while the big clip is still on its way.
    send.sort_by_key(|u| u.size);
    let sent = ssh.upload(&send)?;
    report.uploaded = sent.len();
    report.bytes = sent.iter().map(|u| u.size).fold(0, u64::saturating_add);
    // What arrived: the same size and mtime as sent.
    let media_sent: Vec<&Unit> = sent.iter().filter(|u| !u.font_folder).collect();
    let after = ssh.stat(&media_sent.iter().map(|u| u.dest.as_str()).collect::<Vec<_>>())?;
    for (u, st) in media_sent.iter().zip(after) {
        if st == Some((u.size, u.mtime)) {
            report.confirmed.push(json!({"path": u.src, "size": u.size, "mtime": u.mtime}));
        } else {
            note(&format!("⚠️  {} didn't arrive complete; run mirror again", u.dest));
        }
    }
    for u in sent.iter().filter(|u| u.font_folder) {
        report.confirmed.push(json!({"path": u.src, "size": u.size, "mtime": u.mtime}));
    }
    let _ = host;
    Ok(report)
}

fn mark(b: &mut Backend, confirmed: &[Value], clear: bool) -> Result<(), Failure> {
    b.exec("cloud.markUploaded", json!({"files": confirmed, "clear": clear}))?;
    Ok(())
}

/// Write the env file there and run `cloud.preflight` with the manifest.
fn remote_preflight(ssh: &Ssh, opts: &Opts, m: &Manifest) -> Result<Value, Failure> {
    let locale = if m.host.cjk_locale.is_empty() { m.host.cjk_forms.clone() } else { m.host.cjk_locale.clone() };
    let env_file = &opts.env_file;
    let env = format!(
        "# Written by effectcraft-cli mirror (CR7): `. {env_file}` before effectcraft-cli serve, so fonts and Han glyph forms match the Mac.\nexport EFFECTCRAFT_FONT_DIRS={}\nexport EFFECTCRAFT_CJK_LOCALE={}\n",
        sh_quote(&opts.font_dir),
        sh_quote(&locale)
    );
    let manifest = serde_json::to_vec(m).map_err(|e| Failure::Error(e.to_string()))?;
    let manifest_path = "/tmp/effectcraft-mirror-manifest.json";
    // The env file's path may use $HOME: not quoted.
    ssh.run(
        &format!(
            "mkdir -p \"$(dirname {env_file})\" && cat > {mp} && printf '%s' {env} > {env_file} && echo {MARK}",
            mp = sh_quote(manifest_path),
            env = sh_quote(&env)
        ),
        &manifest,
    )?;
    let params = json!({"manifestPath": manifest_path}).to_string();
    let out = ssh.run(
        &format!(
            "set -a; . {env_file}; set +a; cli={cli}; if [ ! -x \"$cli\" ]; then echo {MARK}; echo '{{\"ok\":false,\"summary\":\"no effectcraft-cli at '\"$cli\"' (build it there: cargo build --profile local -p effectcraft-cli)\"}}'; exit 0; fi; out=$(\"$cli\" exec cloud.preflight --empty --params {params} --json 2>/tmp/effectcraft-mirror-preflight.log); echo {MARK}; echo \"$out\"",
            cli = sh_quote(&opts.remote_cli),
            params = sh_quote(&params)
        ),
        &[],
    )?;
    let text = String::from_utf8_lossy(&out);
    let json_line = text.lines().rev().find(|l| l.trim_start().starts_with('{')).unwrap_or("");
    let v: Value = serde_json::from_str(json_line).map_err(|e| Failure::Error(format!("cloud.preflight on the server: {e} ({})", text.trim())))?;
    // `exec --json` wraps errors as {"error": …}.
    Ok(match v.get("error") {
        Some(e) => json!({"ok": false, "summary": format!("cloud.preflight failed there: {e}")}),
        None => v,
    })
}

fn say_preflight(p: &Value, m: &Manifest) {
    if p.is_null() {
        return;
    }
    if p["ok"].as_bool() == Some(true) {
        say(&format!("✅ cloud preflight: 0 missing ({} files, {} fonts, {} effects)", p["counts"]["files"], p["counts"]["fonts"], p["counts"]["effects"]));
    } else {
        say(&format!("❌ cloud preflight: {}", p["summary"].as_str().unwrap_or("failed")));
        for item in p["missing"].as_array().into_iter().flatten().take(40) {
            say(&format!(
                "   • {} {}: {}",
                item["kind"].as_str().unwrap_or(""),
                item["path"].as_str().or(item["font"].as_str()).unwrap_or(""),
                item["detail"].as_str().unwrap_or("")
            ));
        }
    }
    if !m.nondeterministic.is_empty() {
        say(&format!("🎲 {} expression(s) use Math.random or Date: their cloud frames can differ from the Mac's", m.nondeterministic.len()));
    }
}

// ---------------------------------------------------------------- ssh

struct Ssh {
    target: String,
    base: Vec<String>,
}

impl Ssh {
    fn new(target: String, port: Option<&str>, identity: Option<&str>) -> Ssh {
        let mut base: Vec<String> = [
            "-o",
            "BatchMode=yes",
            "-o",
            "ServerAliveInterval=15",
            "-o",
            "ControlMaster=auto",
            "-o",
            "ControlPath=~/.ssh/ec-mirror-%C",
            "-o",
            "ControlPersist=120",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        if let Some(p) = port {
            base.extend(["-p".into(), p.into()]);
        }
        if let Some(i) = identity {
            base.extend(["-o".into(), "IdentitiesOnly=yes".into(), "-i".into(), i.into()]);
        }
        Ssh { target, base }
    }

    fn command(&self, script: &str) -> Command {
        let mut c = Command::new("ssh");
        c.args(&self.base).arg(&self.target).arg(format!("bash -c {}", sh_quote(script)));
        c
    }

    /// Run a script there with `stdin`; its stdout after [`MARK`].
    fn run(&self, script: &str, stdin: &[u8]) -> Result<Vec<u8>, Failure> {
        let mut child = self
            .command(script)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| Failure::Error(format!("ssh: {e}")))?;
        let input = stdin.to_vec();
        let mut pipe = child.stdin.take();
        let writer = std::thread::spawn(move || {
            if let Some(p) = pipe.as_mut() {
                let _ = p.write_all(&input);
            }
            drop(pipe);
        });
        let out = child.wait_with_output().map_err(|e| Failure::Error(format!("ssh: {e}")))?;
        let _ = writer.join();
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            let err: Vec<&str> = err.lines().filter(|l| !l.trim().is_empty()).collect();
            return Err(Failure::Error(format!("ssh {}: {}", self.target, err.iter().rev().take(3).rev().cloned().collect::<Vec<_>>().join(" / "))));
        }
        Ok(after_mark(&out.stdout))
    }

    /// Size and mtime of each path (`None`: not a file there).
    fn stat(&self, paths: &[&str]) -> Result<Vec<Option<(u64, i64)>>, Failure> {
        if paths.is_empty() {
            return Ok(vec![]);
        }
        let out = self.run(
            &format!("echo {MARK}; while IFS= read -r -d '' f; do if [ -f \"$f\" ]; then stat -c '%s %Y' -- \"$f\"; else echo -; fi; done"),
            &nul_list(paths),
        )?;
        let lines: Vec<Option<(u64, i64)>> = String::from_utf8_lossy(&out)
            .lines()
            .map(|l| {
                let mut it = l.split_whitespace();
                Some((it.next()?.parse().ok()?, it.next()?.parse().ok()?))
            })
            .collect();
        if lines.len() != paths.len() {
            return Err(Failure::Error(format!("stat on the server answered {} lines for {} files", lines.len(), paths.len())));
        }
        Ok(lines)
    }

    /// sha256 of each path there (`None`: unreadable).
    fn sha256(&self, paths: &[&str]) -> Result<Vec<Option<String>>, Failure> {
        say(&format!("#️⃣  hashing {} same-size file(s) on the server", paths.len()));
        let out = self.run(
            &format!("echo {MARK}; while IFS= read -r -d '' f; do h=$(sha256sum -- \"$f\" 2>/dev/null | cut -d' ' -f1); echo \"${{h:--}}\"; done"),
            &nul_list(paths),
        )?;
        Ok(String::from_utf8_lossy(&out)
            .lines()
            .map(|l| Some(l.trim().to_ascii_lowercase()).filter(|h| h.len() == 64))
            .chain(std::iter::repeat(None))
            .take(paths.len())
            .collect())
    }

    /// sha256 → path of every font file in the font folder.
    fn font_hashes(&self, dir: &str) -> Result<BTreeMap<String, String>, Failure> {
        let out = self.run(
            &format!(
                "echo {MARK}; d={}; [ -d \"$d\" ] || exit 0; find \"$d\" -type f \\( -iname '*.ttf' -o -iname '*.otf' -o -iname '*.ttc' -o -iname '*.otc' \\) -print0 | xargs -0 -r sha256sum",
                sh_quote(dir)
            ),
            &[],
        )?;
        Ok(String::from_utf8_lossy(&out)
            .lines()
            .filter_map(|l| {
                let (h, p) = l.split_once("  ")?;
                Some((h.trim().to_ascii_lowercase(), p.to_string()))
            })
            .collect())
    }

    fn retime(&self, files: &[(&str, i64)]) -> Result<(), Failure> {
        let mut list = vec![];
        for (p, t) in files {
            list.extend_from_slice(t.to_string().as_bytes());
            list.push(0);
            list.extend_from_slice(p.as_bytes());
            list.push(0);
        }
        self.run(&format!("while IFS= read -r -d '' t && IFS= read -r -d '' f; do touch -m -d \"@$t\" -- \"$f\"; done; echo {MARK}"), &list)?;
        Ok(())
    }

    fn free_bytes(&self) -> Result<u64, Failure> {
        let out = self.run(&format!("echo {MARK}; df -B1 --output=avail / | tail -1"), &[])?;
        String::from_utf8_lossy(&out).trim().parse().map_err(|_| Failure::Error("cannot read the server's free space".into()))
    }

    /// Stream the files as one tar archive into `tar -x` there. Returns the files sent whole.
    fn upload(&self, files: &[Unit]) -> Result<Vec<Unit>, Failure> {
        let mut child = self
            .command(&format!("tar -xf - -C / --no-same-owner && echo {MARK} && echo done"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| Failure::Error(format!("ssh: {e}")))?;
        let Some(mut pipe) = child.stdin.take() else { return Err(Failure::Error("ssh: no stdin".into())) };
        let total: u64 = files.iter().map(|u| u.size).fold(0, u64::saturating_add);
        let started = Instant::now();
        let mut last = Instant::now();
        let mut done = 0u64;
        let mut sent = vec![];
        let mut buf = vec![0u8; 1 << 20];
        let mut broken: Option<String> = None;
        'files: for u in files {
            let Ok(mut f) = std::fs::File::open(&u.src) else {
                note(&format!("⚠️  cannot read {} (skipped)", u.src));
                continue;
            };
            let header = tar_header(&u.dest, u.size, u.mtime);
            if let Err(e) = pipe.write_all(&header) {
                broken = Some(e.to_string());
                break;
            }
            let mut left = u.size;
            let mut whole = true;
            while left > 0 {
                let want = usize::try_from(left.min(buf.len() as u64)).unwrap_or(buf.len());
                let Some(chunk) = buf.get_mut(..want) else { break };
                let n = match f.read(chunk) {
                    Ok(0) | Err(_) => {
                        // Shorter than its header says (it changed): pad so the archive stays valid.
                        whole = false;
                        chunk.fill(0);
                        want
                    }
                    Ok(n) => n,
                };
                if let Err(e) = pipe.write_all(buf.get(..n).unwrap_or(&[])) {
                    broken = Some(e.to_string());
                    break 'files;
                }
                left = left.saturating_sub(n as u64);
                done = done.saturating_add(n as u64);
                if last.elapsed() > Duration::from_secs(1) {
                    last = Instant::now();
                    let rate = done as f64 / started.elapsed().as_secs_f64().max(0.001);
                    let eta = total.saturating_sub(done) as f64 / rate.max(1.0);
                    progress(&format!(
                        "⬆️  {} / {} · {:.1} MB/s · ETA {}:{:02} · {}",
                        gb(done),
                        gb(total),
                        rate / 1e6,
                        (eta / 60.0) as u64,
                        (eta % 60.0) as u64,
                        short(&u.src)
                    ));
                }
            }
            let pad = (512 - (u.size % 512) as usize) % 512;
            if let Err(e) = pipe.write_all(&vec![0u8; pad]) {
                broken = Some(e.to_string());
                break;
            }
            if whole && cloud_assets::stat(&u.src) == Some((u.size, u.mtime)) {
                sent.push(u.clone());
            } else {
                note(&format!("⚠️  {} changed while it was sent; run mirror again", u.src));
            }
        }
        if broken.is_none() {
            let _ = pipe.write_all(&[0u8; 1024]);
        }
        drop(pipe);
        let out = child.wait_with_output().map_err(|e| Failure::Error(format!("ssh: {e}")))?;
        progress_end();
        if let Some(e) = broken.filter(|_| !out.status.success()) {
            let err = String::from_utf8_lossy(&out.stderr);
            return Err(Failure::Error(format!("upload stopped ({e}): {}", err.lines().last().unwrap_or(""))));
        }
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            return Err(Failure::Error(format!("tar on the server failed: {}", err.lines().rfind(|l| !l.is_empty()).unwrap_or(""))));
        }
        say(&format!("⬆️  sent {} file(s), {} in {:.0} s", sent.len(), gb(done), started.elapsed().as_secs_f64()));
        Ok(sent)
    }
}

fn after_mark(out: &[u8]) -> Vec<u8> {
    let text = String::from_utf8_lossy(out);
    match text.find(MARK) {
        Some(i) => text.get(i + MARK.len()..).unwrap_or("").trim_start_matches(['\r', '\n']).as_bytes().to_vec(),
        None => out.to_vec(),
    }
}

fn nul_list(paths: &[&str]) -> Vec<u8> {
    let mut v = vec![];
    for p in paths {
        v.extend_from_slice(p.as_bytes());
        v.push(0);
    }
    v
}

/// Single-quoted for a POSIX shell.
fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

// ---------------------------------------------------------------- tar (POSIX pax)

/// The header block(s) of one regular file at absolute `path` (stored relative to `/`). Long or
/// non-ASCII names and sizes over the ustar limit get a pax extended header first.
fn tar_header(path: &str, size: u64, mtime: i64) -> Vec<u8> {
    let name = path.trim_start_matches('/');
    let mut out = vec![];
    let mut pax = String::new();
    if name.len() > 99 || !name.is_ascii() {
        pax.push_str(&pax_record("path", name));
    }
    const USTAR_MAX: u64 = 0o77777777777;
    if size > USTAR_MAX {
        pax.push_str(&pax_record("size", &size.to_string()));
    }
    if !pax.is_empty() {
        out.extend(ustar(b"././@PaxHeader", pax.len() as u64, mtime, b'x'));
        out.extend_from_slice(pax.as_bytes());
        out.resize(out.len() + (512 - pax.len() % 512) % 512, 0);
    }
    let short: Vec<u8> = name.bytes().filter(u8::is_ascii).take(99).collect();
    out.extend(ustar(&short, size.min(USTAR_MAX), mtime, b'0'));
    out
}

/// `"<len> key=value\n"`, where len counts the whole record.
fn pax_record(key: &str, value: &str) -> String {
    let body = format!(" {key}={value}\n");
    let mut len = body.len() + 1;
    while format!("{len}{body}").len() != len {
        len += 1;
    }
    format!("{len}{body}")
}

fn ustar(name: &[u8], size: u64, mtime: i64, kind: u8) -> [u8; 512] {
    let mut h = [0u8; 512];
    let put = |h: &mut [u8; 512], at: usize, bytes: &[u8]| {
        for (i, b) in bytes.iter().enumerate() {
            if let Some(slot) = h.get_mut(at + i) {
                *slot = *b;
            }
        }
    };
    put(&mut h, 0, name.get(..name.len().min(100)).unwrap_or(&[]));
    put(&mut h, 100, b"0000644\0");
    put(&mut h, 108, b"0000000\0");
    put(&mut h, 116, b"0000000\0");
    put(&mut h, 124, format!("{size:011o}\0").as_bytes());
    put(&mut h, 136, format!("{:011o}\0", mtime.max(0)).as_bytes());
    put(&mut h, 148, b"        ");
    put(&mut h, 156, &[kind]);
    put(&mut h, 257, b"ustar\0");
    put(&mut h, 263, b"00");
    let sum: u32 = h.iter().map(|b| u32::from(*b)).sum();
    put(&mut h, 148, format!("{sum:06o}\0 ").as_bytes());
    h
}

// ---------------------------------------------------------------- output

fn say(s: &str) {
    let _ = writeln!(std::io::stderr(), "{s}");
}

fn note(s: &str) {
    progress_end();
    let _ = writeln!(std::io::stderr(), "{s}");
}

fn progress(s: &str) {
    use std::io::IsTerminal;
    // A log file gets the summary lines only.
    if !std::io::stderr().is_terminal() {
        return;
    }
    let _ = write!(std::io::stderr(), "\r\x1b[2K{s}");
    let _ = std::io::stderr().flush();
}

fn progress_end() {
    use std::io::IsTerminal;
    if !std::io::stderr().is_terminal() {
        return;
    }
    let _ = write!(std::io::stderr(), "\r\x1b[2K");
}

fn gb(bytes: u64) -> String {
    let b = bytes as f64;
    if b >= 1e9 {
        format!("{:.2} GB", b / 1e9)
    } else if b >= 1e6 {
        format!("{:.1} MB", b / 1e6)
    } else {
        format!("{:.0} KB", (b / 1e3).ceil())
    }
}

/// The last two path components.
fn short(path: &str) -> String {
    let parts: Vec<&str> = path.rsplit('/').take(2).collect();
    parts.into_iter().rev().collect::<Vec<_>>().join("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pax_record_length_counts_itself() {
        let r = pax_record("path", "a");
        assert_eq!(r, "9 path=a\n");
        let long = pax_record("path", &"x".repeat(95));
        assert_eq!(long.len().to_string(), long.split(' ').next().unwrap_or(""));
    }

    #[test]
    fn headers_are_valid_ustar_and_long_names_get_pax() {
        let h = tar_header("/Users/elias/clip.mov", 5, 1_700_000_000);
        assert_eq!(h.len(), 512);
        assert_eq!(&h[..20], b"Users/elias/clip.mov");
        assert_eq!(&h[257..263], b"ustar\0");
        let stored: u32 = u32::from_str_radix(std::str::from_utf8(&h[148..154]).unwrap(), 8).unwrap();
        let mut copy = h.clone();
        copy[148..156].copy_from_slice(b"        ");
        assert_eq!(stored, copy.iter().map(|b| u32::from(*b)).sum::<u32>());
        let long = format!("/Users/elias/{}/ä clip.mov", "Dokumente mit Leerzeichen ".repeat(6));
        let h = tar_header(&long, 5, 0);
        assert_eq!(h[156], b'x');
        assert_eq!(h.len(), 512 * 3);
        assert!(String::from_utf8_lossy(&h[512..1024]).contains(&format!("path={}", long.trim_start_matches('/'))));
    }

    #[test]
    fn quoting_survives_quotes_and_spaces() {
        assert_eq!(sh_quote("10-10 Edit's.mov"), "'10-10 Edit'\\''s.mov'");
    }

    /// The archive this writes extracts with the system tar, names with spaces and all.
    #[cfg(unix)]
    #[test]
    fn system_tar_extracts_the_stream() {
        let dir = std::env::temp_dir().join(format!("ec-mirror-tar-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);
        let name = format!("{}/sub dir/{}clip one.mov", dir.display(), "long ".repeat(25));
        let mut archive = tar_header(&name, 3, 1_600_000_000);
        archive.extend_from_slice(b"abc");
        archive.resize(archive.len() + 509, 0);
        archive.extend_from_slice(&[0u8; 1024]);
        let out = dir.join("out");
        let _ = std::fs::create_dir_all(&out);
        let mut tar = Command::new("tar").args(["-xf", "-", "-C"]).arg(&out).stdin(Stdio::piped()).spawn().unwrap();
        tar.stdin.take().unwrap().write_all(&archive).unwrap();
        assert!(tar.wait().unwrap().success());
        let got = out.join(name.trim_start_matches('/'));
        assert_eq!(std::fs::read(&got).unwrap(), b"abc");
        assert_eq!(cloud_assets::stat(&got.to_string_lossy()).map(|s| s.1), Some(1_600_000_000));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
