//! `effectcraft-cli figma-bridge`: the local HTTP endpoint the EffectCraft Figma plug-in posts
//! its selection to (a plug-in's UI can only make HTTP requests; the app's control channel is
//! JSON lines over TCP and deliberately rejects HTTP). The bridge checks the request's host,
//! origin and pairing code, writes the payload to a temporary file and runs `file.importFigma
//! {payloadPath}` in the running app over the control channel (one undo step per transfer).
//!
//! ```text
//! GET  /status   → {ok, bridge, paired, effectcraft: {connected, error?}}
//! POST /import   {payload, target?, frameRate?, duration?}  (header X-EffectCraft-Code) → the command's result
//! ```
//!
//! Security: it listens on 127.0.0.1 only and the `Host` header must name the loopback address
//! (no DNS rebinding). Origins: the plug-in iframe (`null`), figma.com, or none (curl, agents);
//! browsers on other sites are refused. Origin `null` is any sandboxed iframe, not only the
//! plug-in, so the pairing code printed at start-up is the real protection: 12 characters from
//! the OS's CSPRNG (60 bits), and after 5 wrong codes in a row the bridge refuses codes for a
//! minute (429). Request bodies are capped at 64 MB. The code is kept in the settings folder,
//! created readable only by the user, so it survives restarts.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use effectcraft_automation::BridgeClient;
use serde_json::{Value, json};

const MAX_HEAD: usize = 32 * 1024;
/// The largest import the bridge accepts. Big selections with many embedded images (PDF + PNG,
/// base64) can reach tens of megabytes; send those in parts (integrations/figma/README.md).
const MAX_BODY: usize = 64 * 1024 * 1024;
const MAX_CONNECTIONS: usize = 8;
const CODE_FILE: &str = "figma-bridge-code";
/// The pairing code's alphabet: no 0/O, 1/I.
const CODE_ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
/// The pairing code's length: 12 characters of 5 bits, 60 bits.
const CODE_LEN: usize = 12;
/// After this many wrong pairing codes in a row, the bridge refuses codes for [`LOCKOUT`].
const MAX_WRONG_CODES: u32 = 5;
const LOCKOUT: Duration = Duration::from_secs(60);
const TOO_LARGE: &str = "payload too large";

pub struct Config {
    pub port: u16,
    pub control: String,
    pub code: Option<String>,
}

/// A pairing code: [`CODE_LEN`] characters from [`CODE_ALPHABET`], drawn from the OS's
/// cryptographic random number generator (rejection sampling, so every character is equally
/// likely). Errors only when the OS has no random source.
fn new_code() -> Result<String, String> {
    let n = CODE_ALPHABET.len();
    // The largest multiple of the alphabet size that fits in a byte: bytes at or above it are
    // dropped (with 32 letters none are, but the code stays right if the alphabet changes).
    let limit = 256 - 256 % n;
    let mut out = String::with_capacity(CODE_LEN);
    let mut buf = [0u8; 32];
    for _ in 0..64 {
        getrandom::fill(&mut buf).map_err(|e| format!("no secure random source for the pairing code ({e})"))?;
        for b in buf {
            if out.len() == CODE_LEN {
                return Ok(out);
            }
            if usize::from(b) < limit
                && let Some(c) = CODE_ALPHABET.get(usize::from(b) % n)
            {
                out.push(char::from(*c));
            }
        }
    }
    if out.len() == CODE_LEN { Ok(out) } else { Err("the random source kept failing".into()) }
}

/// `explicit` (at least 8 characters), else the code stored in `dir`, else a new one (stored in
/// `dir` when there is one, created readable only by the user).
fn pairing_code_in(dir: Option<&std::path::Path>, explicit: Option<&str>) -> Result<String, String> {
    if let Some(c) = explicit.map(str::trim).filter(|c| !c.is_empty()) {
        if c.len() < 8 {
            return Err("the pairing code needs at least 8 characters".into());
        }
        return Ok(c.to_string());
    }
    let path = dir.map(|d| d.join(CODE_FILE));
    if let Some(p) = &path
        && let Ok(c) = std::fs::read_to_string(p)
        && c.trim().len() >= 8
    {
        return Ok(c.trim().to_string());
    }
    let code = new_code()?;
    if let Some(p) = &path {
        if let Some(d) = p.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        // Not stored: the code still works until the bridge quits.
        let _ = effectcraft_engine::figma::write_private(p, code.as_bytes());
    }
    Ok(code)
}

/// The stored pairing code, or a new one (stored when there is a settings folder).
pub fn pairing_code(explicit: Option<&str>) -> Result<String, String> {
    pairing_code_in(effectcraft_host::config_dir().as_deref(), explicit)
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

fn read_request(stream: impl Read) -> Result<Request, String> {
    let mut r = BufReader::new(stream);
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
        return Err(format!("{TOO_LARGE} (the bridge takes up to {} MB; send fewer layers at once)", MAX_BODY >> 20));
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

/// The `Host` header must be the loopback address (a page on another name that resolves to
/// 127.0.0.1, DNS rebinding, is refused). No header (HTTP/1.0 tools) is fine.
fn host_ok(h: Option<&str>, port: u16) -> bool {
    let Some(h) = h else { return true };
    let h = h.trim().to_ascii_lowercase();
    let (name, p) = match h.rsplit_once(':') {
        Some((n, p)) if !n.is_empty() && !p.contains(']') => (n.to_string(), p.parse::<u16>().ok()),
        _ => (h.clone(), Some(80)),
    };
    matches!(name.as_str(), "127.0.0.1" | "localhost" | "[::1]") && p == Some(port)
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

/// Wrong pairing codes: after [`MAX_WRONG_CODES`] in a row, codes are refused for a while (any
/// sandboxed page can reach the bridge with origin `null`, so the code must not be guessable).
#[derive(Default)]
struct Guesses {
    wrong: u32,
    locked_until: Option<Instant>,
}

/// What a request's `X-EffectCraft-Code` says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Auth {
    /// No code (or an empty one).
    None,
    Paired,
    Wrong,
    /// Too many wrong codes: not checked.
    Locked,
}

struct Shared {
    control: String,
    code: String,
    port: u16,
    /// One transfer at a time.
    lock: Mutex<()>,
    guesses: Mutex<Guesses>,
    lockout: Duration,
}

impl Shared {
    fn new(control: String, code: String, port: u16) -> Self {
        Self { control, code, port, lock: Mutex::new(()), guesses: Mutex::new(Guesses::default()), lockout: LOCKOUT }
    }

    /// Check the request's pairing code, counting wrong ones: the [`MAX_WRONG_CODES`]th wrong
    /// code in a row locks codes out for `lockout`; a right one resets the count.
    fn auth(&self, req: &Request) -> Auth {
        let Some(c) = req.header("x-effectcraft-code").map(str::trim).filter(|c| !c.is_empty()) else {
            return Auth::None;
        };
        let mut g = self.guesses.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(t) = g.locked_until {
            if Instant::now() < t {
                return Auth::Locked;
            }
            *g = Guesses::default();
        }
        if same_code(c, &self.code) {
            g.wrong = 0;
            return Auth::Paired;
        }
        g.wrong = g.wrong.saturating_add(1);
        if g.wrong >= MAX_WRONG_CODES {
            g.locked_until = Instant::now().checked_add(self.lockout);
        }
        Auth::Wrong
    }
}

fn status(sh: &Shared, auth: Auth) -> Value {
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
    json!({"ok": true, "bridge": env!("CARGO_PKG_VERSION"), "paired": auth == Auth::Paired, "effectcraft": app})
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

/// The checks before a request runs: host, origin and the pairing code (None: go ahead).
fn refuse(sh: &Shared, req: &Request) -> Option<(&'static str, Value)> {
    if !host_ok(req.header("host"), sh.port) {
        return Some(("403 Forbidden", json!({"ok": false, "error": "host not allowed"})));
    }
    if !origin_ok(req.header("origin")) {
        return Some(("403 Forbidden", json!({"ok": false, "error": "origin not allowed"})));
    }
    let path = req.path.split('?').next().unwrap_or("");
    if (req.method.as_str(), path) == ("POST", "/import") {
        match sh.auth(req) {
            Auth::Paired => {}
            Auth::Locked => return Some(("429 Too Many Requests", json!({"ok": false, "error": "too many wrong pairing codes; try again in a minute"}))),
            Auth::None | Auth::Wrong => {
                return Some(("401 Unauthorized", json!({"ok": false, "error": "wrong or missing pairing code (printed by `effectcraft-cli figma-bridge`)"})));
            }
        }
    }
    None
}

fn handle(stream: TcpStream, sh: &Shared) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(30)));
    let req = match stream.try_clone().map_err(|e| e.to_string()).and_then(read_request) {
        Ok(r) => r,
        Err(e) => {
            let status = if e.starts_with(TOO_LARGE) { "413 Payload Too Large" } else { "400 Bad Request" };
            respond(&stream, status, None, &json!({"ok": false, "error": e}));
            return;
        }
    };
    // CORS headers only for allowed origins on an allowed host.
    let origin = req.header("origin").filter(|_| host_ok(req.header("host"), sh.port)).map(str::to_string);
    let origin = origin.as_deref();
    if let Some((st, body)) = refuse(sh, &req) {
        respond(&stream, st, origin, &body);
        return;
    }
    let path = req.path.split('?').next().unwrap_or("");
    match (req.method.as_str(), path) {
        ("OPTIONS", _) => respond(&stream, "204 No Content", origin, &json!({})),
        ("GET", "/status") => respond(&stream, "200 OK", origin, &status(sh, sh.auth(&req))),
        ("POST", "/import") => {
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
    let code = pairing_code(cfg.code.as_deref())?;
    eprintln!("EffectCraft Figma bridge on http://127.0.0.1:{} → EffectCraft control channel {}", cfg.port, cfg.control);
    eprintln!("Pairing code (paste it into the Figma plug-in once): {code}");
    let shared = Arc::new(Shared::new(cfg.control, code, cfg.port));
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

    fn req(method: &str, path: &str, headers: &[(&str, &str)]) -> Request {
        Request {
            method: method.into(),
            path: path.into(),
            headers: headers.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
            body: br#"{"payload":{"nodes":[]}}"#.to_vec(),
        }
    }

    const PLUGIN: (&str, &str) = ("Origin", "null");
    const HOST: (&str, &str) = ("Host", "127.0.0.1:7789");

    #[test]
    fn origins_hosts_and_codes() {
        assert!(origin_ok(None) && origin_ok(Some("null")) && origin_ok(Some("https://www.figma.com")));
        assert!(!origin_ok(Some("https://evil.example")) && !origin_ok(Some("http://localhost:3000")));
        assert!(host_ok(None, 7789) && host_ok(Some("127.0.0.1:7789"), 7789) && host_ok(Some("localhost:7789"), 7789) && host_ok(Some("[::1]:7789"), 7789));
        assert!(!host_ok(Some("evil.example:7789"), 7789) && !host_ok(Some("127.0.0.1:9877"), 7789) && !host_ok(Some("localhost"), 7789));
        assert!(same_code("ABC", "ABC") && !same_code("ABC", "ABD") && !same_code("AB", "ABC"));
        let c = new_code().unwrap();
        assert_eq!(c.len(), 12);
        assert_ne!(c, new_code().unwrap());
    }

    #[test]
    fn codes_use_the_whole_alphabet_and_nothing_else() {
        let mut seen = [0usize; 256];
        for _ in 0..400 {
            let c = new_code().unwrap();
            assert_eq!(c.len(), CODE_LEN);
            for b in c.bytes() {
                assert!(CODE_ALPHABET.contains(&b), "{c}");
                seen[usize::from(b)] += 1;
            }
        }
        // 4,800 draws over 32 letters: about 150 each; a biased or stuck generator shows.
        for &a in CODE_ALPHABET {
            let n = seen[usize::from(a)];
            assert!((60..300).contains(&n), "{} drawn {n} times", char::from(a));
        }
    }

    #[test]
    fn foreign_hosts_and_origins_are_refused() {
        let sh = Shared::new("127.0.0.1:9877".into(), "CODE12345678".into(), 7789);
        let code = ("X-EffectCraft-Code", "CODE12345678");
        assert!(refuse(&sh, &req("POST", "/import", &[PLUGIN, HOST, code])).is_none());
        let (st, _) = refuse(&sh, &req("POST", "/import", &[("Origin", "https://evil.example"), HOST, code])).unwrap();
        assert!(st.starts_with("403"));
        // DNS rebinding: an allowed-looking request on another host name.
        let (st, _) = refuse(&sh, &req("POST", "/import", &[PLUGIN, ("Host", "rebind.example:7789"), code])).unwrap();
        assert!(st.starts_with("403"));
    }

    #[test]
    fn wrong_codes_lock_out_then_recover() {
        let mut sh = Shared::new("127.0.0.1:9877".into(), "CODE12345678".into(), 7789);
        sh.lockout = Duration::from_millis(300);
        let import = |sh: &Shared, code: &str| refuse(sh, &req("POST", "/import", &[PLUGIN, HOST, ("X-EffectCraft-Code", code)])).map(|(st, _)| st);
        assert_eq!(import(&sh, "WRONG1234567"), Some("401 Unauthorized"));
        assert_eq!(refuse(&sh, &req("POST", "/import", &[PLUGIN, HOST])).map(|(st, _)| st), Some("401 Unauthorized"));
        // A right code resets the count.
        for _ in 0..3 {
            assert_eq!(import(&sh, "WRONG1234567"), Some("401 Unauthorized"));
        }
        assert_eq!(import(&sh, "CODE12345678"), None);
        for _ in 0..4 {
            assert_eq!(import(&sh, "WRONG1234567"), Some("401 Unauthorized"));
        }
        // The fifth wrong one in a row locks codes out, even the right one; /status can't probe.
        assert_eq!(import(&sh, "WRONG1234567"), Some("401 Unauthorized"));
        assert_eq!(import(&sh, "CODE12345678"), Some("429 Too Many Requests"));
        assert_eq!(sh.auth(&req("GET", "/status", &[PLUGIN, HOST, ("X-EffectCraft-Code", "CODE12345678")])), Auth::Locked);
        std::thread::sleep(Duration::from_millis(350));
        assert_eq!(import(&sh, "CODE12345678"), None);
        // No code at all isn't a guess.
        for _ in 0..10 {
            assert_eq!(sh.auth(&req("GET", "/status", &[PLUGIN, HOST])), Auth::None);
        }
        assert_eq!(import(&sh, "CODE12345678"), None);
    }

    #[test]
    fn oversized_bodies_are_refused_before_reading() {
        let head = format!("POST /import HTTP/1.1\r\nHost: 127.0.0.1:7789\r\nContent-Length: {}\r\n\r\n{{}}", MAX_BODY + 1);
        let e = read_request(head.as_bytes()).err().unwrap();
        assert!(e.starts_with(TOO_LARGE), "{e}");
        let ok = "POST /import HTTP/1.1\r\nContent-Length: 2\r\n\r\n{}";
        assert_eq!(read_request(ok.as_bytes()).unwrap().body, b"{}");
        const { assert!(MAX_BODY <= 64 * 1024 * 1024) };
    }

    #[test]
    fn pairing_code_is_stored_private_and_reused() {
        let dir = std::env::temp_dir().join(format!("ec-figma-code-{}", new_code().unwrap()));
        let a = pairing_code_in(Some(&dir), None).unwrap();
        assert_eq!(pairing_code_in(Some(&dir), None).unwrap(), a);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(std::fs::metadata(dir.join(CODE_FILE)).unwrap().permissions().mode() & 0o777, 0o600);
        }
        assert_eq!(pairing_code_in(Some(&dir), Some(" MINE-CODE ")).unwrap(), "MINE-CODE");
        assert!(pairing_code_in(Some(&dir), Some("SHORT")).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
