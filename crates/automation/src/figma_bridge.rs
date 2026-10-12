//! The Figma bridge: the local HTTP endpoint the EffectCraft Figma plug-in posts its selection
//! to. A plug-in's UI can only make HTTP requests, and the app's control channel is JSON lines
//! over TCP that deliberately rejects HTTP, so this small server sits between them.
//!
//! Two hosts use it: the desktop app runs it in-process at launch (Settings ▸ Scripting &
//! Expressions ▸ Figma Bridge) and imports straight into the open session, and
//! `effectcraft-cli figma-bridge` runs it standalone and forwards to the app's control channel.
//! What happens on the far side is a [`FigmaHost`].
//!
//! ```text
//! GET  /status   → {ok, bridge, host, pairing, paired, effectcraft: {connected, error?}}
//! POST /pair     {client?}  → {ok, code} once the person clicks Allow in EffectCraft
//! POST /import   {payload, target?, frameRate?, duration?, scale?}  (header X-EffectCraft-Code)
//!                → {ok, result} (the `file.importFigma` reply)
//! ```
//!
//! Security: it listens on 127.0.0.1 only; the `Host` header must name the loopback address (no
//! DNS rebinding); browsers may call it only from the plug-in iframe (origin `null`) or figma.com;
//! and every import needs the pairing code. The code is handed out by `/pair` only after the
//! person allows it in EffectCraft (one prompt at a time, with a cool-down after Deny), or is
//! read from the settings folder / printed by the CLI. Origin `null` is any sandboxed iframe, not
//! only the plug-in, so the code is the real protection: 12 characters from the OS's CSPRNG (60
//! bits), and after 5 wrong codes in a row the bridge refuses codes for a minute (429). Request
//! bodies are capped at 64 MB. The code is kept in the settings folder, created readable only by
//! the user, so it survives restarts and both hosts share it.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// The port the plug-in's manifest allows.
pub const DEFAULT_PORT: u16 = 7789;
const MAX_HEAD: usize = 32 * 1024;
/// The largest import the bridge accepts. Big selections with many embedded images (PDF + PNG,
/// base64) can reach tens of megabytes; send those in parts (integrations/figma/README.md).
pub const MAX_BODY: usize = 64 * 1024 * 1024;
const MAX_CONNECTIONS: usize = 8;
/// After this many wrong pairing codes in a row, the bridge refuses codes for [`LOCKOUT`].
const MAX_WRONG_CODES: u32 = 5;
const LOCKOUT: Duration = Duration::from_secs(60);
/// The pairing code's file in the settings folder.
pub const CODE_FILE: &str = "figma-bridge-code";
/// After a Deny, further pairing requests are refused for this long.
const DENY_COOLDOWN: Duration = Duration::from_secs(10);

/// What the person answered to a pairing request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pairing {
    Allowed,
    Denied,
    /// This host can't ask (the CLI bridge): use the printed code.
    Unsupported,
    /// The prompt couldn't be shown or wasn't answered in time.
    Failed,
}

/// The side of the bridge that does the work.
pub trait FigmaHost: Send + Sync + 'static {
    /// Short name for `/status` (`app` or `cli`).
    fn name(&self) -> &'static str;
    /// `{connected, error?}`: whether imports can run now.
    fn status(&self) -> Value;
    /// Run `file.importFigma` with these params (`payload` holds the node JSON).
    fn import(&self, params: Value) -> Result<Value, String>;
    /// Ask the person whether `client` may send layers (blocks until they answer).
    fn pair(&self, client: &str) -> Pairing;
}

/// The pairing code's alphabet: no 0/O, 1/I.
pub const CODE_ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
/// The pairing code's length: 12 characters of 5 bits, 60 bits.
pub const CODE_LEN: usize = 12;

/// A pairing code: [`CODE_LEN`] characters from [`CODE_ALPHABET`], drawn from the OS's
/// cryptographic random number generator (rejection sampling, so every character is equally
/// likely). Errors only when the OS has no random source.
pub fn new_code() -> Result<String, String> {
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

/// `explicit`, else the code stored in `dir`, else a new one (stored in `dir` when there is one,
/// readable only by the user: created with mode 0600 on unix).
pub fn pairing_code(dir: Option<&Path>, explicit: Option<&str>) -> Result<String, String> {
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
        // Not stored: the code still works until the host quits.
        let _ = effectcraft_engine::figma::write_private(p, code.as_bytes());
    }
    Ok(code)
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

const TOO_LARGE: &str = "payload too large";

fn read_request(stream: impl Read) -> Result<Request, String> {
    let mut r = BufReader::new(stream);
    let mut head = Vec::new();
    loop {
        let mut line = Vec::new();
        let room = MAX_HEAD.saturating_sub(head.len()) as u64 + 1;
        let n = (&mut r).take(room).read_until(b'\n', &mut line).map_err(|e| e.to_string())?;
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
pub fn origin_ok(o: Option<&str>) -> bool {
    match o {
        None => true,
        Some(o) => o == "null" || o == "https://www.figma.com" || o == "https://figma.com",
    }
}

/// The `Host` header must be the loopback address (a page on another name that resolves to
/// 127.0.0.1, DNS rebinding, is refused). No header (HTTP/1.0 tools) is fine.
pub fn host_ok(h: Option<&str>, port: u16) -> bool {
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

/// Constant-time comparison.
pub fn same_code(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Pairing requests: one prompt at a time, none for a while after a Deny.
#[derive(Default)]
struct Gate {
    asking: bool,
    denied_at: Option<Instant>,
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

/// One bridge: a host, its pairing code and port.
pub struct Bridge {
    host: Arc<dyn FigmaHost>,
    code: String,
    port: u16,
    /// One transfer at a time.
    lock: Mutex<()>,
    gate: Mutex<Gate>,
    guesses: Mutex<Guesses>,
    lockout: Duration,
}

/// A reply: HTTP status and JSON body.
type Reply = (&'static str, Value);

impl Bridge {
    pub fn new(host: Arc<dyn FigmaHost>, code: String, port: u16) -> Self {
        Self { host, code, port, lock: Mutex::new(()), gate: Mutex::new(Gate::default()), guesses: Mutex::new(Guesses::default()), lockout: LOCKOUT }
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

    fn locked() -> Reply {
        ("429 Too Many Requests", json!({"ok": false, "error": "too many wrong pairing codes; try again in a minute"}))
    }

    fn status(&self, auth: Auth) -> Value {
        json!({
            "ok": true,
            "bridge": env!("CARGO_PKG_VERSION"),
            "host": self.host.name(),
            "pairing": self.host.name() == "app",
            "paired": auth == Auth::Paired,
            "effectcraft": self.host.status(),
        })
    }

    fn pair(&self, req: &Request, auth: Auth) -> Reply {
        match auth {
            Auth::Paired => return ("200 OK", json!({"ok": true, "code": self.code})),
            Auth::Locked => return Self::locked(),
            Auth::None | Auth::Wrong => {}
        }
        let body: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
        let client: String = body
            .get("client")
            .and_then(Value::as_str)
            .unwrap_or("Figma plug-in")
            .chars()
            .filter(|c| !c.is_control())
            .take(60)
            .collect::<String>()
            .trim()
            .to_string();
        let client = if client.is_empty() { "Figma plug-in".to_string() } else { client };
        {
            let mut g = self.gate.lock().unwrap_or_else(PoisonError::into_inner);
            if g.asking {
                return ("409 Conflict", json!({"ok": false, "error": "EffectCraft is already asking; answer the prompt there"}));
            }
            if g.denied_at.is_some_and(|t| t.elapsed() < DENY_COOLDOWN) {
                return ("429 Too Many Requests", json!({"ok": false, "error": "denied in EffectCraft; try again in a few seconds"}));
            }
            g.asking = true;
        }
        let answer = self.host.pair(&client);
        let mut g = self.gate.lock().unwrap_or_else(PoisonError::into_inner);
        g.asking = false;
        match answer {
            Pairing::Allowed => ("200 OK", json!({"ok": true, "code": self.code})),
            Pairing::Denied => {
                g.denied_at = Some(Instant::now());
                ("403 Forbidden", json!({"ok": false, "error": "denied in EffectCraft"}))
            }
            Pairing::Unsupported => (
                "501 Not Implemented",
                json!({"ok": false, "error": "this bridge pairs with the code printed by `effectcraft-cli figma-bridge`; or turn on Settings ▸ Scripting & Expressions ▸ Figma Bridge in EffectCraft"}),
            ),
            Pairing::Failed => ("504 Gateway Timeout", json!({"ok": false, "error": "EffectCraft didn't answer; click Connect again"})),
        }
    }

    fn import(&self, req: &Request, auth: Auth) -> Reply {
        match auth {
            Auth::Paired => {}
            Auth::Locked => return Self::locked(),
            Auth::None | Auth::Wrong => {
                return ("401 Unauthorized", json!({"ok": false, "error": "not paired with EffectCraft (wrong or missing pairing code)"}));
            }
        }
        if !req.header("content-type").is_some_and(|c| c.starts_with("application/json") || c.starts_with("text/plain")) {
            return ("415 Unsupported Media Type", json!({"ok": false, "error": "send JSON"}));
        }
        let body: Value = match serde_json::from_slice(&req.body) {
            Ok(v) => v,
            Err(e) => return ("400 Bad Request", json!({"ok": false, "error": format!("not JSON: {e}")})),
        };
        let Some(payload) = body.get("payload").filter(|p| p.is_object() || p.is_array()) else {
            return ("400 Bad Request", json!({"ok": false, "error": "missing `payload`"}));
        };
        let mut params = json!({"payload": payload});
        for k in ["target", "frameRate", "duration", "scale"] {
            if let Some(v) = body.get(k).filter(|v| !v.is_null()) {
                params[k] = v.clone();
            }
        }
        let _guard = self.lock.lock().unwrap_or_else(PoisonError::into_inner);
        match self.host.import(params) {
            Ok(r) => ("200 OK", json!({"ok": true, "result": r})),
            Err(e) => ("502 Bad Gateway", json!({"ok": false, "error": e})),
        }
    }

    /// Answer one parsed request (everything but the transport).
    fn route(&self, req: &Request) -> Reply {
        if !host_ok(req.header("host"), self.port) {
            return ("403 Forbidden", json!({"ok": false, "error": "host not allowed"}));
        }
        if !origin_ok(req.header("origin")) {
            return ("403 Forbidden", json!({"ok": false, "error": "origin not allowed"}));
        }
        let path = req.path.split('?').next().unwrap_or("");
        match (req.method.as_str(), path) {
            ("OPTIONS", _) => ("204 No Content", json!({})),
            ("GET", "/status") => ("200 OK", self.status(self.auth(req))),
            ("POST", "/pair") => self.pair(req, self.auth(req)),
            ("POST", "/import") => self.import(req, self.auth(req)),
            _ => ("404 Not Found", json!({"ok": false, "error": "GET /status, POST /pair or POST /import"})),
        }
    }

    fn handle(&self, stream: TcpStream) {
        let _ = stream.set_read_timeout(Some(Duration::from_secs(30)));
        let req = match stream.try_clone().map_err(|e| e.to_string()).and_then(read_request) {
            Ok(r) => r,
            Err(e) => {
                let status = if e.starts_with(TOO_LARGE) { "413 Payload Too Large" } else { "400 Bad Request" };
                respond(&stream, status, None, &json!({"ok": false, "error": e}));
                return;
            }
        };
        let (status, body) = self.route(&req);
        // CORS headers only for allowed origins on an allowed host.
        let origin = req.header("origin").filter(|_| host_ok(req.header("host"), self.port));
        respond(&stream, status, origin, &body);
    }

    /// Serve `listener` until it fails (blocks).
    pub fn serve(self: Arc<Self>, listener: TcpListener) {
        let open = Arc::new(AtomicUsize::new(0));
        for stream in listener.incoming().flatten() {
            if open.fetch_add(1, Ordering::SeqCst) >= MAX_CONNECTIONS {
                open.fetch_sub(1, Ordering::SeqCst);
                respond(&stream, "503 Service Unavailable", None, &json!({"ok": false, "error": "busy"}));
                continue;
            }
            let (me, open) = (self.clone(), open.clone());
            std::thread::spawn(move || {
                me.handle(stream);
                open.fetch_sub(1, Ordering::SeqCst);
            });
        }
    }
}

/// Listen on `127.0.0.1:port`.
pub fn bind(port: u16) -> Result<TcpListener, String> {
    TcpListener::bind(("127.0.0.1", port)).map_err(|e| format!("cannot listen on 127.0.0.1:{port} ({e})"))
}

/// Start a bridge on its own thread (the desktop app). Returns the port it listens on.
pub fn spawn(host: Arc<dyn FigmaHost>, code: String, port: u16) -> Result<u16, String> {
    let listener = bind(port)?;
    let port = listener.local_addr().map(|a| a.port()).unwrap_or(port);
    let bridge = Arc::new(Bridge::new(host, code, port));
    std::thread::Builder::new().name("figma-bridge".into()).spawn(move || bridge.serve(listener)).map_err(|e| e.to_string())?;
    Ok(port)
}

/// The CLI's host: forwards to the app's control channel ([`crate::BridgeClient`]). The payload
/// goes through a temporary file (control-channel lines are bounded).
pub struct ControlHost {
    pub control: String,
}

impl FigmaHost for ControlHost {
    fn name(&self) -> &'static str {
        "cli"
    }

    fn status(&self) -> Value {
        match crate::BridgeClient::new(&self.control).and_then(|mut c| c.call("engine.execute", json!({"command": "figma.status"}))) {
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
        }
    }

    fn import(&self, mut params: Value) -> Result<Value, String> {
        let payload = params.get_mut("payload").map(Value::take).unwrap_or(Value::Null);
        let dir = std::env::temp_dir().join("effectcraft-figma-bridge");
        std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        let file = dir.join(format!("payload-{}-{stamp}.json", std::process::id()));
        std::fs::write(&file, payload.to_string()).map_err(|e| format!("cannot write the payload: {e}"))?;
        if let Some(o) = params.as_object_mut() {
            o.remove("payload");
            o.insert("payloadPath".into(), json!(file.to_string_lossy()));
        }
        let r = crate::BridgeClient::new(&self.control)
            .and_then(|mut c| c.call("engine.execute", json!({"command": "file.importFigma", "params": params})))
            .map_err(|e| e.to_string());
        let _ = std::fs::remove_file(&file);
        r
    }

    fn pair(&self, _client: &str) -> Pairing {
        Pairing::Unsupported
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fake {
        answer: Pairing,
        imported: Mutex<Vec<Value>>,
        asked: AtomicUsize,
    }

    impl FigmaHost for Fake {
        fn name(&self) -> &'static str {
            "app"
        }
        fn status(&self) -> Value {
            json!({"connected": true})
        }
        fn import(&self, params: Value) -> Result<Value, String> {
            self.imported.lock().unwrap().push(params);
            Ok(json!({"native": 3, "images": 1}))
        }
        fn pair(&self, _client: &str) -> Pairing {
            self.asked.fetch_add(1, Ordering::SeqCst);
            self.answer
        }
    }

    fn bridge(answer: Pairing) -> (Bridge, Arc<Fake>) {
        let fake = Arc::new(Fake { answer, imported: Mutex::new(vec![]), asked: AtomicUsize::new(0) });
        (Bridge::new(fake.clone(), "CODE12345678".into(), 7789), fake)
    }

    fn req(method: &str, path: &str, headers: &[(&str, &str)], body: &str) -> Request {
        Request {
            method: method.into(),
            path: path.into(),
            headers: headers.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
            body: body.as_bytes().to_vec(),
        }
    }

    const PLUGIN: (&str, &str) = ("Origin", "null");
    const HOST: (&str, &str) = ("Host", "127.0.0.1:7789");
    const JSON: (&str, &str) = ("Content-Type", "application/json");

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
    fn wrong_codes_lock_out_then_recover() {
        let (mut b, fake) = bridge(Pairing::Allowed);
        b.lockout = Duration::from_millis(300);
        let body = r#"{"payload":{"nodes":[]}}"#;
        let import = |b: &Bridge, code: &str| b.route(&req("POST", "/import", &[PLUGIN, HOST, JSON, ("X-EffectCraft-Code", code)], body)).0;
        // A right code resets the count.
        for _ in 0..4 {
            assert!(import(&b, "WRONG1234567").starts_with("401"));
        }
        assert!(import(&b, "CODE12345678").starts_with("200"));
        for _ in 0..4 {
            assert!(import(&b, "WRONG1234567").starts_with("401"));
        }
        // The fifth wrong one in a row locks codes out, even the right one, and /pair too.
        assert!(import(&b, "WRONG1234567").starts_with("401"));
        assert!(import(&b, "CODE12345678").starts_with("429"));
        assert!(import(&b, "WRONG1234567").starts_with("429"));
        let (st, r) = b.route(&req("POST", "/pair", &[PLUGIN, HOST, ("X-EffectCraft-Code", "CODE12345678")], ""));
        assert!(st.starts_with("429") && r.get("code").is_none(), "{st} {r}");
        let (_, r) = b.route(&req("GET", "/status", &[PLUGIN, HOST, ("X-EffectCraft-Code", "CODE12345678")], ""));
        assert_eq!(r["paired"], false);
        assert_eq!(fake.imported.lock().unwrap().len(), 1);
        assert_eq!(fake.asked.load(Ordering::SeqCst), 0);
        std::thread::sleep(Duration::from_millis(350));
        assert!(import(&b, "CODE12345678").starts_with("200"));
        // No code at all isn't a guess.
        for _ in 0..10 {
            let (st, _) = b.route(&req("GET", "/status", &[PLUGIN, HOST], ""));
            assert!(st.starts_with("200"));
        }
        assert!(import(&b, "CODE12345678").starts_with("200"));
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
    fn import_needs_the_code() {
        let (b, fake) = bridge(Pairing::Denied);
        let body = r#"{"payload":{"nodes":[]},"target":"newComp"}"#;
        let (st, r) = b.route(&req("POST", "/import", &[PLUGIN, HOST, JSON], body));
        assert!(st.starts_with("401"), "{st} {r}");
        let (st, r) = b.route(&req("POST", "/import", &[PLUGIN, HOST, JSON, ("X-EffectCraft-Code", "WRONG1234567")], body));
        assert!(st.starts_with("401"), "{st} {r}");
        assert!(fake.imported.lock().unwrap().is_empty());
        let (st, r) = b.route(&req("POST", "/import", &[PLUGIN, HOST, JSON, ("X-EffectCraft-Code", "CODE12345678")], body));
        assert!(st.starts_with("200"), "{st} {r}");
        assert_eq!(r["result"]["native"], 3);
        let got = fake.imported.lock().unwrap();
        assert_eq!(got[0]["target"], "newComp");
        assert!(got[0]["payload"]["nodes"].is_array());
    }

    #[test]
    fn foreign_origins_and_hosts_are_refused() {
        let (b, fake) = bridge(Pairing::Allowed);
        let code = ("X-EffectCraft-Code", "CODE12345678");
        let body = r#"{"payload":{"nodes":[]}}"#;
        let (st, _) = b.route(&req("POST", "/import", &[("Origin", "https://evil.example"), HOST, JSON, code], body));
        assert!(st.starts_with("403"));
        let (st, _) = b.route(&req("POST", "/pair", &[("Origin", "https://evil.example"), HOST, JSON], "{}"));
        assert!(st.starts_with("403"));
        // DNS rebinding: an allowed-looking request on another host name.
        let (st, _) = b.route(&req("POST", "/import", &[PLUGIN, ("Host", "rebind.example:7789"), JSON, code], body));
        assert!(st.starts_with("403"));
        assert!(fake.imported.lock().unwrap().is_empty());
        assert_eq!(fake.asked.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn pairing_hands_out_the_code_only_when_allowed() {
        let (b, fake) = bridge(Pairing::Allowed);
        let (st, r) = b.route(&req("POST", "/pair", &[PLUGIN, HOST, JSON], r#"{"client":"Figma: Card file"}"#));
        assert!(st.starts_with("200"), "{st}");
        assert_eq!(r["code"], "CODE12345678");
        assert_eq!(fake.asked.load(Ordering::SeqCst), 1);
        // Already paired: no second prompt.
        let (st, _) = b.route(&req("POST", "/pair", &[PLUGIN, HOST, ("X-EffectCraft-Code", "CODE12345678")], ""));
        assert!(st.starts_with("200"));
        assert_eq!(fake.asked.load(Ordering::SeqCst), 1);

        let (b, fake) = bridge(Pairing::Denied);
        let (st, r) = b.route(&req("POST", "/pair", &[PLUGIN, HOST], ""));
        assert!(st.starts_with("403") && r.get("code").is_none(), "{st} {r}");
        // Cool-down: no new prompt right after a Deny.
        let (st, _) = b.route(&req("POST", "/pair", &[PLUGIN, HOST], ""));
        assert!(st.starts_with("429"));
        assert_eq!(fake.asked.load(Ordering::SeqCst), 1);

        let (b, _) = bridge(Pairing::Unsupported);
        let (st, r) = b.route(&req("POST", "/pair", &[PLUGIN, HOST], ""));
        assert!(st.starts_with("501") && r.get("code").is_none());
    }

    #[test]
    fn status_says_whether_paired() {
        let (b, _) = bridge(Pairing::Allowed);
        let (_, r) = b.route(&req("GET", "/status", &[PLUGIN, HOST], ""));
        assert_eq!(r["paired"], false);
        assert_eq!(r["pairing"], true);
        let (_, r) = b.route(&req("GET", "/status", &[PLUGIN, HOST, ("X-EffectCraft-Code", "CODE12345678")], ""));
        assert_eq!(r["paired"], true);
        assert_eq!(r["effectcraft"]["connected"], true);
    }

    #[test]
    fn pairing_code_is_stored_and_reused() {
        let dir = std::env::temp_dir().join(format!("ec-figma-code-{}", new_code().unwrap()));
        let a = pairing_code(Some(&dir), None).unwrap();
        assert_eq!(pairing_code(Some(&dir), None).unwrap(), a);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(std::fs::metadata(dir.join(CODE_FILE)).unwrap().permissions().mode() & 0o777, 0o600);
        }
        assert_eq!(pairing_code(Some(&dir), Some(" MINE-CODE ")).unwrap(), "MINE-CODE");
        assert!(pairing_code(Some(&dir), Some("SHORT")).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn over_http() {
        let (b, fake) = bridge(Pairing::Allowed);
        let listener = bind(0).unwrap();
        let port = listener.local_addr().unwrap().port();
        let b = Arc::new(Bridge { port, ..b });
        std::thread::spawn(move || b.serve(listener));
        let body = r#"{"payload":{"nodes":[]}}"#;
        let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        write!(
            s,
            "POST /import HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nOrigin: null\r\nContent-Type: application/json\r\nX-EffectCraft-Code: CODE12345678\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).unwrap();
        assert!(out.starts_with("HTTP/1.1 200"), "{out}");
        assert!(out.contains("Access-Control-Allow-Origin: null"));
        assert_eq!(fake.imported.lock().unwrap().len(), 1);
    }
}
