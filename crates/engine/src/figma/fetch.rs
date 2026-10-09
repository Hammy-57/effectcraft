//! The Figma REST API (`GET /v1/files/:key/nodes`, `GET /v1/images/:key`) through the system's
//! `curl`, as model downloads do (`crate::models`): no networking code is linked into
//! EffectCraft. The personal access token goes to curl on stdin (a `-K -` config), never on
//! the command line, and only to `api.figma.com` (or a loopback test server).

use std::io::Write as _;
use std::process::{Command, Stdio};

pub const API: &str = "https://api.figma.com";

/// A file key and node ids from a Figma link
/// (`https://www.figma.com/design/KEY/Name?node-id=1-2`, `/file/`, `/proto/`, branch links).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FigmaRef {
    pub key: String,
    pub ids: Vec<String>,
}

fn valid_key(k: &str) -> bool {
    (10..=128).contains(&k.len()) && k.chars().all(|c| c.is_ascii_alphanumeric())
}

/// Node ids use `:` in the API and `-` in links (`1-2` → `1:2`; instance ids keep `;`/`I`).
pub fn normalize_id(id: &str) -> Option<String> {
    let id = percent_decode(id.trim());
    if id.is_empty() || id.len() > 512 || !id.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, ':' | '-' | ';' | 'I')) {
        return None;
    }
    Some(if id.contains(':') { id } else { id.replacen('-', ":", 1) })
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let c = b.get(i).copied().unwrap_or(0);
        if c == b'%'
            && let Some(h) = b.get(i + 1..i + 3).and_then(|h| std::str::from_utf8(h).ok()).and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(h);
            i += 3;
            continue;
        }
        out.push(c);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub fn parse_url(url: &str) -> Result<FigmaRef, String> {
    let u = url.trim();
    let rest = u.strip_prefix("https://").or_else(|| u.strip_prefix("http://")).unwrap_or(u);
    let (host_path, query) = rest.split_once('?').unwrap_or((rest, ""));
    let mut parts = host_path.split('/');
    let host = parts.next().unwrap_or("");
    if !(host == "figma.com" || host.ends_with(".figma.com")) {
        return Err(format!("not a Figma link: {url}"));
    }
    let segs: Vec<&str> = parts.filter(|p| !p.is_empty()).collect();
    let kind = segs.first().copied().unwrap_or("");
    if !matches!(kind, "file" | "design" | "proto" | "board") {
        return Err(format!("expected a figma.com/design/<file key>/… link, got {url}"));
    }
    // Branch links: /design/KEY/branch/BRANCH_KEY/Name.
    let key = match (segs.get(2), segs.get(3)) {
        (Some(&"branch"), Some(b)) => b,
        _ => segs.get(1).copied().unwrap_or(""),
    };
    if !valid_key(key) {
        return Err(format!("no file key in {url}"));
    }
    let mut ids = vec![];
    for kv in query.split('&') {
        if let Some(v) = kv.strip_prefix("node-id=") {
            for id in percent_decode(v).split(',') {
                ids.push(normalize_id(id).ok_or_else(|| format!("bad node-id `{id}`"))?);
            }
        }
    }
    Ok(FigmaRef { key: key.to_string(), ids })
}

/// Only Figma's API (HTTPS) or a loopback server (tests, local proxies) gets the token.
pub fn check_api_base(base: &str) -> Result<String, String> {
    let b = base.trim_end_matches('/');
    if b == API {
        return Ok(b.to_string());
    }
    if loopback(b) || loopback(&b.replacen("https://", "http://", 1)) {
        return Ok(b.to_string());
    }
    Err(format!("apiBase must be {API} or a loopback address, not `{base}`"))
}

fn loopback(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("http://") else { return false };
    let host = rest.split('/').next().unwrap_or("");
    let host = host.rsplit_once(':').map(|(h, _)| h).unwrap_or(host);
    matches!(host, "127.0.0.1" | "localhost" | "[::1]")
}

fn check_token(t: &str) -> Result<&str, String> {
    let t = t.trim();
    if t.is_empty() || t.len() > 512 || !t.chars().all(|c| c.is_ascii_graphic() && c != '"' && c != '\\') {
        return Err("the Figma token is not valid (a personal access token from Figma ▸ Settings ▸ Security)".into());
    }
    Ok(t)
}

/// GET `url` with curl; `token` goes in an `X-Figma-Token` header via stdin. Returns the body
/// of a 2xx reply, else an error with Figma's message.
fn curl(url: &str, token: Option<&str>, max_bytes: u64) -> Result<Vec<u8>, String> {
    let tmp = std::env::temp_dir().join(format!(
        "effectcraft-figma-{}-{}.part",
        std::process::id(),
        web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0)
    ));
    let mut cmd = Command::new("curl");
    cmd.args(["-sS", "-L", "--proto", "=https,http", "--connect-timeout", "15", "--max-time", "120", "--max-filesize"])
        .arg(max_bytes.to_string())
        .args(["-w", "%{http_code}", "-o"])
        .arg(&tmp)
        .args(["-K", "-"])
        .arg(url)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| format!("talking to Figma needs curl ({e})"))?;
    if let Some(mut stdin) = child.stdin.take() {
        let cfg = match token {
            Some(t) => format!("header = \"X-Figma-Token: {}\"\n", check_token(t)?),
            None => String::new(),
        };
        stdin.write_all(cfg.as_bytes()).map_err(|e| format!("curl: {e}"))?;
    }
    let out = child.wait_with_output().map_err(|e| format!("curl: {e}"))?;
    let body = std::fs::read(&tmp).unwrap_or_default();
    let _ = std::fs::remove_file(&tmp);
    let code: u32 = String::from_utf8_lossy(&out.stdout).trim().parse().unwrap_or(0);
    if !out.status.success() || !(200..300).contains(&code) {
        let msg = serde_json::from_slice::<serde_json::Value>(&body)
            .ok()
            .and_then(|v| v.get("err").or_else(|| v.get("message")).and_then(|m| m.as_str()).map(str::to_string))
            .unwrap_or_else(|| String::from_utf8_lossy(&out.stderr).trim().to_string());
        let hint = match code {
            403 => " (check the token: Figma ▸ Settings ▸ Security ▸ Personal access tokens, with file content read access)",
            404 => " (no such file, or the token's account can't open it)",
            429 => " (rate limited by Figma; try again in a minute)",
            _ => "",
        };
        return Err(format!("Figma request failed: HTTP {code}{}{hint}", if msg.is_empty() { String::new() } else { format!(": {msg}") }));
    }
    Ok(body)
}

/// `GET /v1/files/:key/nodes?ids=…&geometry=paths`.
pub fn nodes(base: &str, key: &str, ids: &[String], token: &str) -> Result<serde_json::Value, String> {
    if !valid_key(key) {
        return Err(format!("bad file key `{key}`"));
    }
    let ids: Vec<String> = ids.iter().map(|i| normalize_id(i).ok_or_else(|| format!("bad node id `{i}`"))).collect::<Result<_, _>>()?;
    let url = format!("{base}/v1/files/{key}/nodes?ids={}&geometry=paths", ids.join(",").replace(':', "%3A").replace(';', "%3B"));
    let body = curl(&url, Some(token), 512 << 20)?;
    serde_json::from_slice(&body).map_err(|e| format!("Figma sent invalid JSON: {e}"))
}

/// Render `ids` as PNG at `scale` (`GET /v1/images/:key?format=png&use_absolute_bounds=true`)
/// and download each: (id, PNG bytes or the reason it failed).
pub fn images(base: &str, key: &str, ids: &[String], scale: f64, token: &str) -> Result<Vec<(String, Result<Vec<u8>, String>)>, String> {
    let mut out = vec![];
    for chunk in ids.chunks(50) {
        let q: Vec<String> = chunk.iter().filter_map(|i| normalize_id(i)).collect();
        let url =
            format!("{base}/v1/images/{key}?ids={}&format=png&scale={scale}&use_absolute_bounds=true", q.join(",").replace(':', "%3A").replace(';', "%3B"));
        let body = curl(&url, Some(token), 16 << 20)?;
        let v: serde_json::Value = serde_json::from_slice(&body).map_err(|e| format!("Figma sent invalid JSON: {e}"))?;
        if let Some(e) = v.get("err").and_then(|e| e.as_str()) {
            return Err(format!("Figma could not render images: {e}"));
        }
        for id in chunk {
            let link = v.get("images").and_then(|m| m.get(id.as_str())).and_then(|u| u.as_str());
            let r = match link {
                // Image links are pre-signed: no token.
                Some(l) if l.starts_with("https://") || loopback(l) => curl(l, None, 256 << 20),
                Some(l) => Err(format!("unexpected image link {l}")),
                None => Err("Figma rendered no image (empty or invisible node)".into()),
            };
            out.push((id.clone(), r));
        }
    }
    Ok(out)
}

/// Standard base64 (padding optional, whitespace ignored), for PNGs in plug-in payloads.
pub fn base64_decode(s: &str) -> Result<Vec<u8>, String> {
    let s = s.split_once("base64,").map(|(_, b)| b).unwrap_or(s);
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0u32);
    for c in s.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' | b' ' | b'\n' | b'\r' | b'\t' => continue,
            _ => return Err("invalid base64".into()),
        };
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Ok(out)
}
