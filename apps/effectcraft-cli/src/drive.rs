//! `drive-pull --jobs FILE.json`: on the cloud render machine, download big files from Google
//! Drive share links straight to the paths the project expects (CR7). Elias's upload quota costs
//! money, so footage over the upload limit is put on Drive once and every new machine pulls it
//! from there instead of from the Mac.
//!
//! - Share links only ("Anyone with the link"): no password, no OAuth token is asked for or kept.
//! - A file link (`/file/d/<id>/…`, `open?id=`, `uc?id=`) downloads through
//!   `drive.usercontent.google.com` with `confirm=t`; when Drive still answers with its "can't
//!   scan this file for viruses" page, the page's own download form is followed.
//! - A folder link (`/drive/folders/<id>`) is listed (its public folder view, three levels deep)
//!   and the file is found there by name.
//! - The download goes to `<dest>.ec-part`, hashed while it streams; only a file with the Mac's
//!   size (and sha256, when the manifest has it) replaces `dest`, with the Mac's mtime, and its
//!   hash is remembered for later checks.
//!
//! Jobs: `[{link | folders, dest, name, size, mtime, sha256?}]`. Prints `{"results": [{dest, ok, bytes,
//! error?}]}` on stdout, progress on stderr.

use std::io::{Read, Write};
use std::sync::Arc;
use std::time::{Duration, Instant, UNIX_EPOCH};

use effectcraft_engine::cloud_assets;
use serde_json::{Value, json};

use crate::{Args, Failure};

/// The pages Drive answers with are small; a download is not.
const MAX_PAGE: u64 = 4 * 1024 * 1024;

pub fn run(args: &Args) -> Result<(), Failure> {
    let Some(file) = args.opt("--jobs") else { return Err(Failure::Usage("drive-pull --jobs FILE.json".into())) };
    let text = std::fs::read(file).map_err(|e| Failure::Error(format!("{file}: {e}")))?;
    let jobs: Vec<Value> = serde_json::from_slice(&text).map_err(|e| Failure::Error(format!("{file}: {e}")))?;
    let agent = agent();
    let mut results = vec![];
    for job in &jobs {
        let dest = job["dest"].as_str().unwrap_or("").to_string();
        let r = pull(&agent, job);
        match r {
            Ok(bytes) => results.push(json!({"dest": dest, "ok": true, "bytes": bytes})),
            Err(e) => {
                note(&format!("❌ {}: {e}", short(&dest)));
                results.push(json!({"dest": dest, "ok": false, "error": e}));
            }
        }
    }
    crate::emit(&json!({"results": results}), true);
    Ok(())
}

fn agent() -> ureq::Agent {
    let provider = Arc::new(rustls_rustcrypto::provider());
    ureq::Agent::config_builder()
        .tls_config(
            ureq::tls::TlsConfig::builder()
                .provider(ureq::tls::TlsProvider::Rustls)
                .root_certs(ureq::tls::RootCerts::PlatformVerifier)
                .unversioned_rustls_crypto_provider(provider)
                .build(),
        )
        .timeout_connect(Some(Duration::from_secs(20)))
        // Big files take long; a stalled connection still ends.
        .timeout_recv_body(None)
        .timeout_global(None)
        .http_status_as_error(false)
        .build()
        .into()
}

fn pull(agent: &ureq::Agent, job: &Value) -> Result<u64, String> {
    let dest = job["dest"].as_str().filter(|d| d.starts_with('/')).ok_or("no absolute dest")?;
    let lower = dest.to_ascii_lowercase();
    if lower.ends_with(".ecproj") || lower.ends_with(".aep") {
        return Err("project files are never replaced from Drive".into());
    }
    let name = job["name"].as_str().map(str::to_string).unwrap_or_else(|| file_name(dest).to_string());
    let size = job["size"].as_u64();
    let want_sha = job["sha256"].as_str().map(str::to_ascii_lowercase);
    // A file link, or shared folders to find it in by name.
    let folders: Vec<&str> = match job["link"].as_str() {
        Some(link) if !is_folder(link) => vec![],
        Some(link) => vec![link],
        None => job["folders"].as_array().into_iter().flatten().filter_map(Value::as_str).collect(),
    };
    let id = match job["link"].as_str().filter(|l| !is_folder(l)) {
        Some(link) => drive_id(link).ok_or("no file id in the link")?,
        None => {
            let mut found = None;
            for f in &folders {
                let folder = drive_id(f).ok_or("no folder id in the link")?;
                if let Some(id) = find_in_folder(agent, &folder, &name, 0)? {
                    found = Some(id);
                    break;
                }
            }
            found.ok_or_else(|| format!("no file called `{name}` in the shared folder(s)"))?
        }
    };
    note(&format!("⬇️  {name} from Google Drive{}", size.map(|s| format!(" ({})", cloud_assets::human_bytes(s))).unwrap_or_default()));
    let resp = open_download(agent, &id)?;
    let part = format!("{dest}.ec-part");
    if let Some(dir) = std::path::Path::new(dest).parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    }
    let result = stream_to(resp, &part, size);
    let (bytes, sha) = match result {
        Ok(v) => v,
        Err(e) => {
            let _ = std::fs::remove_file(&part);
            return Err(e);
        }
    };
    let fail = |e: String| {
        let _ = std::fs::remove_file(&part);
        Err(e)
    };
    if size.is_some_and(|s| s != bytes) {
        return fail(format!("Drive sent {bytes} bytes, the Mac's file has {} (another version on Drive?)", size.unwrap_or(0)));
    }
    if let Some(want) = &want_sha
        && *want != sha
    {
        return fail("the Drive file's content differs from the Mac's (sha256): upload the current version".into());
    }
    if let Some(m) = job["mtime"].as_i64().filter(|m| *m > 0)
        && let Ok(f) = std::fs::File::options().write(true).open(&part)
    {
        let _ = f.set_modified(UNIX_EPOCH + Duration::from_secs(m.unsigned_abs()));
    }
    std::fs::rename(&part, dest).map_err(|e| format!("cannot move it into place: {e}"))?;
    cloud_assets::remember_hash(dest, &sha);
    note(&format!("✅ {name}: {} verified{}", cloud_assets::human_bytes(bytes), if want_sha.is_some() { " (size + sha256)" } else { " (size)" }));
    Ok(bytes)
}

/// The download response for a Drive file id: the direct link, or the warning page's form.
fn open_download(agent: &ureq::Agent, id: &str) -> Result<ureq::http::Response<ureq::Body>, String> {
    let url = format!("https://drive.usercontent.google.com/download?id={}&export=download&confirm=t", encode(id));
    let mut resp = agent.get(&url).call().map_err(|e| format!("network: {e}"))?;
    for _ in 0..2 {
        let status = resp.status().as_u16();
        let html = resp.headers().get("content-type").and_then(|v| v.to_str().ok()).is_some_and(|t| t.starts_with("text/html"));
        if status == 200 && !html {
            return Ok(resp);
        }
        let page = resp.body_mut().with_config().limit(MAX_PAGE).read_to_string().unwrap_or_default();
        if status == 404 {
            return Err("Drive doesn't know this file (wrong or deleted link)".into());
        }
        if page.contains("accounts.google.com") || status == 401 || (status == 403 && !page.contains("download-form")) {
            return Err("the link isn't public: in Drive, Share ▸ General access ▸ Anyone with the link".into());
        }
        if page.contains("Quota exceeded") || page.contains("Too many users") || status == 429 {
            return Err("Drive says the file was downloaded too often today; try again later".into());
        }
        let Some(next) = confirm_url(&page) else {
            return Err(format!("Drive answered HTTP {status} with a page, not the file"));
        };
        resp = agent.get(&next).call().map_err(|e| format!("network: {e}"))?;
    }
    Err("Drive kept answering with a page instead of the file".into())
}

/// Stream the body into `part`, hashing it; progress every few seconds. Returns (bytes, sha256).
fn stream_to(mut resp: ureq::http::Response<ureq::Body>, part: &str, size: Option<u64>) -> Result<(u64, String), String> {
    let mut out = std::fs::File::create(part).map_err(|e| format!("cannot write {part}: {e}"))?;
    let mut reader = resp.body_mut().as_reader();
    let mut h = cloud_assets::Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    let (mut done, started, mut last) = (0u64, Instant::now(), Instant::now());
    loop {
        let n = match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(format!("download stopped: {e}")),
        };
        let chunk = buf.get(..n).ok_or("read past the buffer")?;
        out.write_all(chunk).map_err(|e| format!("cannot write {part}: {e} (disk full?)"))?;
        h.update(chunk);
        done = done.saturating_add(n as u64);
        if size.is_some_and(|s| done > s.saturating_add(1 << 20)) {
            return Err("Drive is sending more than the Mac's file has (another file?)".into());
        }
        if last.elapsed() > Duration::from_secs(5) {
            last = Instant::now();
            let rate = done as f64 / started.elapsed().as_secs_f64().max(0.001);
            note(&format!(
                "   {} / {} · {:.0} MB/s",
                cloud_assets::human_bytes(done),
                size.map(cloud_assets::human_bytes).unwrap_or_else(|| "?".into()),
                rate / 1e6
            ));
        }
    }
    out.flush().map_err(|e| format!("cannot write {part}: {e}"))?;
    out.sync_all().map_err(|e| format!("cannot write {part}: {e}"))?;
    Ok((done, h.finish_hex()))
}

/// Find `name` in a shared folder (its public view; subfolders up to three levels).
fn find_in_folder(agent: &ureq::Agent, folder: &str, name: &str, depth: u32) -> Result<Option<String>, String> {
    let url = format!("https://drive.google.com/embeddedfolderview?id={}", encode(folder));
    let mut resp = agent.get(&url).call().map_err(|e| format!("network: {e}"))?;
    if resp.status().as_u16() != 200 {
        return Err(format!("cannot list the shared folder (HTTP {}): is it shared with anyone with the link?", resp.status().as_u16()));
    }
    let page = resp.body_mut().with_config().limit(MAX_PAGE).read_to_string().map_err(|e| format!("network: {e}"))?;
    let entries = folder_entries(&page);
    if let Some((id, _, _)) = entries.iter().find(|(_, n, folder)| !folder && n.eq_ignore_ascii_case(name)) {
        return Ok(Some(id.clone()));
    }
    if depth < 3 {
        for (id, _, _) in entries.iter().filter(|(_, _, folder)| *folder) {
            if let Some(hit) = find_in_folder(agent, id, name, depth + 1)? {
                return Ok(Some(hit));
            }
        }
    }
    Ok(None)
}

/// (id, title, is folder) of each entry of Drive's embedded folder view.
fn folder_entries(page: &str) -> Vec<(String, String, bool)> {
    let mut out = vec![];
    for chunk in page.split("id=\"entry-").skip(1) {
        let Some(id) = chunk.split('"').next().filter(|id| !id.is_empty() && id.chars().all(id_char)) else { continue };
        let title = chunk.split_once("flip-entry-title\">").and_then(|(_, rest)| rest.split_once('<')).map(|(t, _)| unescape(t.trim())).unwrap_or_default();
        let head = chunk.split("id=\"entry-").next().unwrap_or(chunk);
        out.push((id.to_string(), title, head.contains("/drive/folders/") || head.contains("/folderview?")));
    }
    out
}

/// The download form on Drive's virus-scan warning page, as a URL.
fn confirm_url(page: &str) -> Option<String> {
    let at = page.find("id=\"download-form\"").or_else(|| page.find("<form"))?;
    let start = page.get(..at)?.rfind("<form").unwrap_or(at);
    let form = page.get(start..)?;
    let form = form.get(..form.find("</form>").unwrap_or(form.len()))?;
    let tag_end = form.find('>')?;
    let action = attr(form.get(..tag_end)?, "action")?;
    let mut query = vec![];
    for input in form.split("<input").skip(1) {
        let tag = input.split('>').next().unwrap_or("");
        if let (Some(name), value) = (attr(tag, "name"), attr(tag, "value").unwrap_or_default()) {
            query.push(format!("{}={}", encode(&name), encode(&value)));
        }
    }
    let sep = if action.contains('?') { '&' } else { '?' };
    Some(if query.is_empty() { action } else { format!("{action}{sep}{}", query.join("&")) })
}

fn attr(tag: &str, name: &str) -> Option<String> {
    let key = format!(" {name}=\"");
    let rest = tag.split_once(&key)?.1;
    Some(unescape(rest.split('"').next()?))
}

fn unescape(s: &str) -> String {
    s.replace("&amp;", "&").replace("&quot;", "\"").replace("&#39;", "'").replace("&lt;", "<").replace("&gt;", ">")
}

fn id_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '-' || c == '_'
}

/// The file or folder id in a Drive share link.
fn drive_id(link: &str) -> Option<String> {
    let link = link.trim();
    let after = ["/file/d/", "/folders/", "id=", "/d/"].iter().find_map(|k| link.split_once(k).map(|(_, rest)| rest))?;
    let id: String = after.chars().take_while(|c| id_char(*c)).collect();
    (id.len() >= 10).then_some(id)
}

fn is_folder(link: &str) -> bool {
    link.contains("/folders/") || link.contains("/folderview")
}

fn encode(s: &str) -> String {
    s.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect()
}

fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn short(path: &str) -> &str {
    file_name(path)
}

fn note(s: &str) {
    let _ = writeln!(std::io::stderr(), "{s}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_come_out_of_every_kind_of_share_link() {
        let id = "1aBcDeFgHiJkLmNoPqRsTuVwXyZ_0123-";
        for link in [
            format!("https://drive.google.com/file/d/{id}/view?usp=sharing"),
            format!("https://drive.google.com/open?id={id}"),
            format!("https://drive.google.com/uc?export=download&id={id}"),
            format!("https://drive.usercontent.google.com/download?id={id}&export=download"),
            format!("https://drive.google.com/drive/folders/{id}?usp=drive_link"),
        ] {
            assert_eq!(drive_id(&link).as_deref(), Some(id), "{link}");
        }
        assert!(is_folder(&format!("https://drive.google.com/drive/folders/{id}")));
        assert_eq!(drive_id("https://drive.google.com/file/d/short/view"), None);
    }

    #[test]
    fn the_virus_scan_page_form_becomes_the_download_url() {
        let page = r#"<html><body><form id="download-form" action="https://drive.usercontent.google.com/download" method="get">
            <input type="submit" id="uc-download-link" class="goog-inline-block jfk-button jfk-button-action" value="Download anyway"/>
            <input type="hidden" name="id" value="1AbCdEfGhIjK"><input type="hidden" name="export" value="download">
            <input type="hidden" name="confirm" value="t"><input type="hidden" name="uuid" value="9f1c-2b&amp;x"></form></body></html>"#;
        assert_eq!(
            confirm_url(page).as_deref(),
            Some("https://drive.usercontent.google.com/download?id=1AbCdEfGhIjK&export=download&confirm=t&uuid=9f1c-2b%26x")
        );
        assert_eq!(confirm_url("<html>no form</html>"), None);
    }

    #[test]
    fn folder_view_entries_are_listed() {
        let page = r#"<div class="flip-entries"><div class="flip-entry" id="entry-1FileIdAAAAAAA" tabindex="0"><div class="flip-entry-info"><a href="https://drive.google.com/file/d/1FileIdAAAAAAA/view?usp=drive_web"><div class="flip-entry-title">10-10 presenter keyed clean.mov</div></a></div></div>
            <div class="flip-entry" id="entry-1FolderIdBBBBBB" tabindex="0"><a href="https://drive.google.com/drive/folders/1FolderIdBBBBBB"><div class="flip-entry-title">Proxies &amp; more</div></a></div></div>"#;
        let e = folder_entries(page);
        assert_eq!(e[0], ("1FileIdAAAAAAA".to_string(), "10-10 presenter keyed clean.mov".to_string(), false));
        assert_eq!(e[1], ("1FolderIdBBBBBB".to_string(), "Proxies & more".to_string(), true));
    }
}
