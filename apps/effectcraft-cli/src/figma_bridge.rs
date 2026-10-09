//! `effectcraft-cli figma-bridge`: the local HTTP endpoint the EffectCraft Figma plug-in posts
//! its selection to (a plug-in's UI can only make HTTP requests; the app's control channel is
//! JSON lines over TCP and deliberately rejects HTTP). The bridge checks the request's origin
//! and pairing code, writes the payload to a temporary file and runs `file.importFigma
//! {payloadPath}` in the running app over the control channel (one undo step per transfer).
//!
//! ```text
//! GET  /status   → {ok, bridge, paired, effectcraft: {connected, error?}}
//! POST /import   {payload, target?, frameRate?, duration?}  (header X-EffectCraft-Code) → the command's result
//! ```
//!
//! Listens on 127.0.0.1 only. Origins: the plug-in iframe (`null`), figma.com, or none (curl,
//! agents); browsers on other sites are refused, and every request but `/status` needs the
//! pairing code printed at start-up (kept in the settings folder so it survives restarts).

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use effectcraft_automation::BridgeClient;
use serde_json::{Value, json};

const MAX_HEAD: usize = 32 * 1024;
const MAX_BODY: usize = 512 * 1024 * 1024;
const MAX_CONNECTIONS: usize = 8;
const CODE_FILE: &str = "figma-bridge-code";

pub struct Config {
    pub port: u16,
    pub control: String,
    pub code: Option<String>,
}

/// A pairing code: 12 characters from an unambiguous alphabet, seeded by the OS (through
/// `RandomState`).
fn new_code() -> String {
    use std::hash::{BuildHasher, Hasher};
    const ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    let mut out = String::new();
    for i in 0..12u64 {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u64(i);
        h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0));
        let n = (h.finish() % ALPHABET.len() as u64) as usize;
        out.push(ALPHABET.get(n).copied().map(char::from).unwrap_or('A'));
    }
    out
}

/// The stored pairing code, or a new one (stored when there is a settings folder).
pub fn pairing_code(explicit: Option<&str>) -> String {
    if let Some(c) = explicit.map(str::trim).filter(|c| !c.is_empty()) {
        return c.to_string();
    }
    let path = effectcraft_host::config_dir().map(|d| d.join(CODE_FILE));
    if let Some(p) = &path
        && let Ok(c) = std::fs::read_to_string(p)
        && c.trim().len() >= 8
    {
        return c.trim().to_string();
    }
    let code = new_code();
    if let Some(p) = &path {
        if let Some(d) = p.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        if std::fs::write(p, &code).is_ok() {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                let _ = std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o600));
            }
        }
    }
    code
}

struct Request {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Request {
    fn header(&self, k: &str) -> Option<&str> {
        self.headers.iter().find(|(h, _)| h.eq_ignore_ascii_case(k)).map(|(_, v)| v.as_str())
    }
}

fn read_request(stream: &TcpStream) -> Result<Request, String> {
    let mut r = BufReader::new(stream.try_clone().map_err(|e| e.to_string())?);
    let mut head = Vec::new();
    loop {
        let mut line = Vec::new();
        let n = (&mut r).take((MAX_HEAD - head.len().min(MAX_HEAD)) as u64 + 1).read_until(b'\n', &mut line).map_err(|e| e.to_string())?;
        if n == 0 {
            return Err("connection closed".into());
        }
        head.extend_from_slice(&line);
        if head.len() > MAX_HEAD {
            return Err("request head too large".into());
        }
        if line == b"\r\n" || line == b"\n" {
            break;
        }
    }
    let text = String::from_utf8_lossy(&head).into_owned();
    let mut lines = text.lines();
    let first = lines.next().unwrap_or("");
    let mut parts = first.split_whitespace();
    let (method, path) = (parts.next().unwrap_or("").to_string(), parts.next().unwrap_or("").to_string());
    let headers: Vec<(String, String)> = lines.filter_map(|l| l.split_once(':')).map(|(k, v)| (k.trim().to_string(), v.trim().to_string())).collect();
    let len: usize = headers.iter().find(|(k, _)| k.eq_ignore_ascii_case("content-length")).and_then(|(_, v)| v.parse().ok()).unwrap_or(0);
    if len > MAX_BODY {
        return Err("payload too large".into());
    }
    let mut body = Vec::new();
    r.take(len as u64).read_to_end(&mut body).map_err(|e| e.to_string())?;
    if body.len() != len {
        return Err("truncated body".into());
    }
    Ok(Request { method, path, headers, body })
}

/// Origins allowed to call the bridge: the plug-in UI (an opaque `null` origin), figma.com, or
/// no browser at all.
fn origin_ok(o: Option<&str>) -> bool {
    match o {
        None => true,
        Some(o) => o == "null" || o == "https://www.figma.com" || o == "https://figma.com",
    }
}

fn respond(mut s: &TcpStream, status: &str, origin: Option<&str>, body: &Value) {
    let text = body.to_string();
    let cors = match origin.filter(|o| origin_ok(Some(o))) {
        Some(o) => format!(
            "Access-Control-Allow-Origin: {o}\r\nAccess-Control-Allow-Methods: GET, POST, OPTIONS\r\nAccess-Control-Allow-Headers: content-type, x-effectcraft-code\r\nAccess-Control-Allow-Private-Network: true\r\nAccess-Control-Max-Age: 600\r\nVary: Origin\r\n"
        ),
        None => String::new(),
    };
    let _ = write!(s, "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n{cors}Connection: close\r\n\r\n{text}", text.len());
    let _ = s.flush();
}

fn same_code(a: &str, b: &str) -> bool {
    // Constant-time comparison.
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

struct Shared {
    control: String,
    code: String,
    /// One transfer at a time.
    lock: Mutex<()>,
}

fn status(sh: &Shared, req: &Request) -> Value {
    let paired = req.header("x-effectcraft-code").is_some_and(|c| same_code(c.trim(), &sh.code));
    let app = match BridgeClient::new(&sh.control).and_then(|mut c| c.call("engine.execute", json!({"command": "figma.status"}))) {
        Ok(_) => json!({"connected": true}),
        Err(e) => {
            let e = e.to_string();
            // An app without the command is still connected (but too old to import).
            if e.contains("unknown command") {
                json!({"connected": true, "error": "this EffectCraft has no file.importFigma; update it"})
            } else {
                json!({"connected": false, "error": e})
            }
        }
    };
    json!({"ok": true, "bridge": env!("CARGO_PKG_VERSION"), "paired": paired, "effectcraft": app})
}

fn import(sh: &Shared, req: &Request) -> Result<Value, (String, String)> {
    let body: Value = serde_json::from_slice(&req.body).map_err(|e| ("400 Bad Request".into(), format!("not JSON: {e}")))?;
    let payload =
        body.get("payload").filter(|p| p.is_object() || p.is_array()).ok_or_else(|| ("400 Bad Request".to_string(), "missing `payload`".to_string()))?;
    let dir = std::env::temp_dir().join("effectcraft-figma-bridge");
    std::fs::create_dir_all(&dir).map_err(|e| ("500 Internal Server Error".to_string(), format!("cannot create {}: {e}", dir.display())))?;
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let file = dir.join(format!("payload-{}-{stamp}.json", std::process::id()));
    std::fs::write(&file, payload.to_string()).map_err(|e| ("500 Internal Server Error".to_string(), format!("cannot write the payload: {e}")))?;
    let mut params = json!({"payloadPath": file.to_string_lossy()});
    for k in ["target", "frameRate", "duration", "scale"] {
        if let Some(v) = body.get(k).filter(|v| !v.is_null()) {
            params[k] = v.clone();
        }
    }
    let _guard = sh.lock.lock().unwrap_or_else(PoisonError::into_inner);
    let r = BridgeClient::new(&sh.control)
        .and_then(|mut c| c.call("engine.execute", json!({"command": "file.importFigma", "params": params})))
        .map_err(|e| ("502 Bad Gateway".to_string(), e.to_string()));
    let _ = std::fs::remove_file(&file);
    r
}

fn handle(stream: TcpStream, sh: &Shared) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(30)));
    let req = match read_request(&stream) {
        Ok(r) => r,
        Err(e) => {
            respond(&stream, "400 Bad Request", None, &json!({"ok": false, "error": e}));
            return;
        }
    };
    let origin = req.header("origin").map(str::to_string);
    let origin = origin.as_deref();
    if !origin_ok(origin) {
        respond(&stream, "403 Forbidden", None, &json!({"ok": false, "error": "origin not allowed"}));
        return;
    }
    let path = req.path.split('?').next().unwrap_or("");
    match (req.method.as_str(), path) {
        ("OPTIONS", _) => respond(&stream, "204 No Content", origin, &json!({})),
        ("GET", "/status") => respond(&stream, "200 OK", origin, &status(sh, &req)),
        ("POST", "/import") => {
            if !req.header("x-effectcraft-code").is_some_and(|c| same_code(c.trim(), &sh.code)) {
                respond(
                    &stream,
                    "401 Unauthorized",
                    origin,
                    &json!({"ok": false, "error": "wrong or missing pairing code (printed by `effectcraft-cli figma-bridge`)"}),
                );
                return;
            }
            if !req.header("content-type").is_some_and(|c| c.starts_with("application/json") || c.starts_with("text/plain")) {
                respond(&stream, "415 Unsupported Media Type", origin, &json!({"ok": false, "error": "send JSON"}));
                return;
            }
            match import(sh, &req) {
                Ok(r) => respond(&stream, "200 OK", origin, &json!({"ok": true, "result": r})),
                Err((st, e)) => respond(&stream, &st, origin, &json!({"ok": false, "error": e})),
            }
        }
        _ => respond(&stream, "404 Not Found", origin, &json!({"ok": false, "error": "GET /status or POST /import"})),
    }
}

pub fn serve(cfg: Config) -> Result<(), String> {
    let listener = TcpListener::bind(("127.0.0.1", cfg.port)).map_err(|e| format!("cannot listen on 127.0.0.1:{} ({e})", cfg.port))?;
    // Validate the control address up front (loopback only).
    BridgeClient::new(&cfg.control).map_err(|e| e.to_string())?;
    let code = pairing_code(cfg.code.as_deref());
    eprintln!("EffectCraft Figma bridge on http://127.0.0.1:{} → EffectCraft control channel {}", cfg.port, cfg.control);
    eprintln!("Pairing code (paste it into the Figma plug-in once): {code}");
    let shared = Arc::new(Shared { control: cfg.control, code, lock: Mutex::new(()) });
    let open = Arc::new(AtomicUsize::new(0));
    for stream in listener.incoming().flatten() {
        if open.fetch_add(1, Ordering::SeqCst) >= MAX_CONNECTIONS {
            open.fetch_sub(1, Ordering::SeqCst);
            respond(&stream, "503 Service Unavailable", None, &json!({"ok": false, "error": "busy"}));
            continue;
        }
        let (sh, open) = (shared.clone(), open.clone());
        std::thread::spawn(move || {
            handle(stream, &sh);
            open.fetch_sub(1, Ordering::SeqCst);
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origins_and_codes() {
        assert!(origin_ok(None) && origin_ok(Some("null")) && origin_ok(Some("https://www.figma.com")));
        assert!(!origin_ok(Some("https://evil.example")) && !origin_ok(Some("http://localhost:3000")));
        assert!(same_code("ABC", "ABC") && !same_code("ABC", "ABD") && !same_code("AB", "ABC"));
        let c = new_code();
        assert_eq!(c.len(), 12);
        assert_ne!(c, new_code());
    }
}
