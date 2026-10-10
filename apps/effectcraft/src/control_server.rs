//! Localhost JSON-lines control server (one request per line, one reply per line). Loopback only.
//!
//! The server only ever reads requests: a line that is not a JSON object with a string `method`
//! (an HTTP request line from a browser's cross-origin `fetch`, a stray `nc`, binary junk) gets
//! one error reply and the connection is closed, so nothing after it is executed. Lines are
//! bounded ([`MAX_LINE`]) and so is the number of open connections ([`MAX_CONNECTIONS`]); junk
//! never reaches the UI thread. See `docs/control-protocol.md`.
//!
//! Robustness for agents that retry: a request that times out before the UI thread started it is
//! cancelled (it never runs, and the reply says so), and a request carrying a top-level
//! `requestId` is answered from a small cache when the same id comes again (on any connection),
//! so a retry never runs a command twice. `events.subscribe` makes the connection also receive
//! `{"event": …}` lines when the project or selection changes.
//!
//! Composition claims (`comp.claim`) are released when the last connection that used their
//! `client` closes, so an agent that goes away doesn't keep comps locked until the claims expire.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
#[cfg(test)]
use std::sync::mpsc::channel;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use effectcraft_ui_egui::ControlRequest;
use effectcraft_ui_egui::control::ControlEvents;
use serde_json::{Value, json};

/// Longest accepted request line (bytes, without the newline). Requests are small JSON objects;
/// anything longer is refused and the connection closed.
pub const MAX_LINE: usize = 4 * 1024 * 1024;
/// Connections served at once; further ones get an error line and are closed.
pub const MAX_CONNECTIONS: usize = 32;
/// How long a request may wait for the UI thread.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
/// `requestId`s remembered (with their replies) for retries.
const REPLY_CACHE: usize = 128;
/// Larger replies (rendered frames, inspections) aren't kept: a retry runs again, which is
/// harmless for the reads that return them.
const MAX_CACHED_REPLY: usize = 256 * 1024;
/// Distinct `client` names one connection is tracked for (their claims are released when it
/// closes); longer names than the engine accepts aren't tracked.
const MAX_CLIENTS_PER_CONNECTION: usize = 64;
const MAX_CLIENT_LEN: usize = 128;
/// Change events a connection can receive (`events.subscribe`).
const EVENTS: [&str; 2] = ["project.changed", "selection.changed"];

/// A started control server: the change-event hub it publishes to and the port it bound.
pub struct Started {
    pub events: ControlEvents,
    pub port: u16,
}

/// Listen on `127.0.0.1:port`, sending requests into `tx` (the app's control queue, shared with
/// the Figma bridge). A port that can't be bound is an error the app shows the user.
pub fn start(port: u16, ctx: egui::Context, tx: Sender<ControlRequest>) -> Result<Started, String> {
    let listener = TcpListener::bind(("127.0.0.1", port)).map_err(|e| bind_error(port, &e))?;
    let port = listener.local_addr().map(|a| a.port()).unwrap_or(port);
    let events = ControlEvents::default();
    let shared = Shared { tx, ctx, events: events.clone(), replies: Arc::default(), clients: Arc::default(), timeout: REQUEST_TIMEOUT };
    std::thread::spawn(move || accept_loop(listener, shared));
    Ok(Started { events, port })
}

/// What the user sees when the control port can't be opened.
fn bind_error(port: u16, e: &std::io::Error) -> String {
    if e.kind() == std::io::ErrorKind::AddrInUse {
        format!("Agent control port {port} is already in use (another EffectCraft?): set EFFECTCRAFT_CONTROL_PORT or --control to a free port")
    } else {
        format!("Agent control port {port} could not be opened ({e}): set EFFECTCRAFT_CONTROL_PORT or --control to another port")
    }
}

/// State shared by every connection of one server.
#[derive(Clone)]
struct Shared {
    tx: Sender<ControlRequest>,
    ctx: egui::Context,
    events: ControlEvents,
    replies: Arc<Mutex<ReplyCache>>,
    /// Open connections that sent each `client` name.
    clients: Arc<Mutex<BTreeMap<String, usize>>>,
    timeout: Duration,
}

fn accept_loop(listener: TcpListener, shared: Shared) {
    let open = Arc::new(AtomicUsize::new(0));
    for mut stream in listener.incoming().flatten() {
        if open.fetch_add(1, Ordering::SeqCst) >= MAX_CONNECTIONS {
            open.fetch_sub(1, Ordering::SeqCst);
            let _ = write_line(&mut stream, &json!({"ok": false, "error": "too many control connections"}));
            continue;
        }
        let shared = shared.clone();
        let open = Arc::clone(&open);
        std::thread::spawn(move || {
            serve(stream, &shared);
            open.fetch_sub(1, Ordering::SeqCst);
        });
    }
}

fn serve(stream: TcpStream, shared: &Shared) {
    let Ok(read) = stream.try_clone() else { return };
    let Ok(closer) = stream.try_clone() else { return };
    let out = SharedOut(Arc::new(Mutex::new(stream)));
    // The event stream's sender, kept to wake its writer thread when the connection closes.
    let mut subscribed: Option<Sender<Value>> = None;
    let mut used = BTreeSet::new();
    let events_out = out.clone();
    serve_lines(read, out, |method, params, request_id| {
        note_client(shared, &mut used, &method, &params);
        if method == "events.subscribe" {
            let revision = match &subscribed {
                // Already streaming: one stream per connection.
                Some(_) => shared.events.subscribe().1,
                None => {
                    let (tx, rx) = std::sync::mpsc::channel();
                    forward_events(rx, events_out.clone());
                    let revision = shared.events.attach(tx.clone());
                    subscribed = Some(tx);
                    revision
                }
            };
            return Some(json!({"ok": true, "result": {"subscribed": true, "revision": revision, "events": EVENTS}}));
        }
        dispatch(shared, method, params, request_id.as_deref())
    });
    if let Some(tx) = subscribed {
        let _ = tx.send(Value::Null);
    }
    let _ = closer.shutdown(std::net::Shutdown::Both);
    release_clients(shared, used);
}

/// The `client` an engine request runs as (`engine.execute {command, params: {client}}`).
fn request_client<'a>(method: &str, params: &'a Value) -> Option<&'a str> {
    if !matches!(method, "engine.execute" | "ui.menu.invoke") {
        return None;
    }
    params.get("params")?.get("client")?.as_str().filter(|c| !c.is_empty() && c.len() <= MAX_CLIENT_LEN)
}

/// Remember that this connection runs as the request's `client`.
fn note_client(shared: &Shared, used: &mut BTreeSet<String>, method: &str, params: &Value) {
    let Some(c) = request_client(method, params) else { return };
    if used.contains(c) || used.len() >= MAX_CLIENTS_PER_CONNECTION {
        return;
    }
    used.insert(c.to_string());
    let mut clients = lock(&shared.clients);
    let n = clients.entry(c.to_string()).or_insert(0);
    *n = n.saturating_add(1);
}

/// A connection closed: release the claims of the clients no other open connection uses.
fn release_clients(shared: &Shared, used: BTreeSet<String>) {
    let gone: Vec<String> = {
        let mut clients = lock(&shared.clients);
        used.into_iter()
            .filter(|c| match clients.get_mut(c) {
                Some(n) if *n > 1 => {
                    *n -= 1;
                    false
                }
                Some(_) => {
                    clients.remove(c);
                    true
                }
                None => false,
            })
            .collect()
    };
    for c in &gone {
        let (req, _reply) = ControlRequest::new("engine.execute", json!({"command": "comp.release", "params": {"client": c}}));
        if shared.tx.send(req).is_err() {
            return;
        }
    }
    if !gone.is_empty() {
        shared.ctx.request_repaint();
    }
}

/// Write change events to a subscribed connection until it closes (`null` on the channel: the
/// connection's reader wakes it then, so it sleeps between events instead of polling).
fn forward_events<W: Write + Send + 'static>(rx: Receiver<Value>, mut out: SharedOut<W>) {
    std::thread::spawn(move || {
        while let Ok(event) = rx.recv() {
            if event.is_null() || write_line(&mut out, &event).is_err() {
                return;
            }
        }
    });
}

/// Run one request on the UI thread (or answer a repeated `requestId` from the cache). `None`
/// when the app is gone.
fn dispatch(shared: &Shared, method: String, params: Value, request_id: Option<&str>) -> Option<Value> {
    if let Some(rid) = request_id {
        match lock(&shared.replies).begin(rid) {
            Lookup::Hit(reply) => return Some(reply),
            Lookup::Running => {
                return Some(json!({"ok": false, "error": format!("request `{rid}` is still running; retry later with the same requestId to get its reply")}));
            }
            Lookup::Miss => {}
        }
    }
    let (req, rrx) = ControlRequest::new(method, params);
    let state = req.state.clone();
    if shared.tx.send(req).is_err() {
        if let Some(rid) = request_id {
            lock(&shared.replies).finish(rid, None);
        }
        return None;
    }
    shared.ctx.request_repaint();
    let secs = shared.timeout.as_secs();
    let (reply, keep) = match rrx.recv_timeout(shared.timeout) {
        Ok(v) => (v, true),
        // Cancelled before it started: it never runs, so a retry (same requestId) may run it.
        Err(_) if state.cancel() => (
            json!({"ok": false, "notRun": true, "error": format!("timeout: the app did not start the request within {secs} s; it was NOT run and is safe to retry")}),
            false,
        ),
        Err(_) => (
            json!({"ok": false, "error": format!("timeout: the request started but did not finish within {secs} s; it may still complete (check the project before retrying)")}),
            true,
        ),
    };
    if let Some(rid) = request_id {
        lock(&shared.replies).finish(rid, keep.then(|| reply.clone()));
    }
    Some(reply)
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The last [`REPLY_CACHE`] `requestId`s and their replies (`None` while running).
#[derive(Default)]
struct ReplyCache {
    entries: VecDeque<(String, Option<Value>)>,
}

enum Lookup {
    Hit(Value),
    Running,
    Miss,
}

impl ReplyCache {
    /// Look `id` up; a new id is recorded as running.
    fn begin(&mut self, id: &str) -> Lookup {
        if let Some((_, r)) = self.entries.iter().find(|(k, _)| k == id) {
            return r.clone().map_or(Lookup::Running, Lookup::Hit);
        }
        while self.entries.len() >= REPLY_CACHE {
            self.entries.pop_front();
        }
        self.entries.push_back((id.to_string(), None));
        Lookup::Miss
    }

    /// Store `id`'s reply, or forget the id (`None`: it didn't run, so a retry may run it).
    fn finish(&mut self, id: &str, reply: Option<Value>) {
        let reply = reply.filter(|r| r.to_string().len() <= MAX_CACHED_REPLY);
        let at = self.entries.iter().position(|(k, _)| k == id);
        match (at, reply) {
            (Some(i), Some(r)) => {
                if let Some(e) = self.entries.get_mut(i) {
                    e.1 = Some(r);
                }
            }
            (Some(i), None) => {
                self.entries.remove(i);
            }
            (None, _) => {}
        }
    }
}

/// A writer shared by a connection's reply and event threads; each `write` is a whole line, so
/// lines never interleave.
struct SharedOut<W>(Arc<Mutex<W>>);

impl<W> Clone for SharedOut<W> {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl<W: Write> Write for SharedOut<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let mut w = lock(&self.0);
        w.write_all(buf)?;
        w.flush()?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        lock(&self.0).flush()
    }
}

/// Write `v` and a newline in one `write_all`.
fn write_line(out: &mut impl Write, v: &Value) -> std::io::Result<()> {
    let mut line = v.to_string();
    line.push('\n');
    out.write_all(line.as_bytes())?;
    out.flush()
}

/// One parsed line.
enum Line {
    Request {
        id: Value,
        method: String,
        params: Value,
        /// Top-level `requestId` (string or number): retries with it are answered from the cache.
        request_id: Option<String>,
    },
    Blank,
    /// Not a request: reply with this error and close the connection.
    Reject(String),
}

fn parse_line(bytes: &[u8]) -> Line {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return Line::Reject("not a control request (invalid UTF-8); closing the connection".into());
    };
    let text = text.trim();
    if text.is_empty() {
        return Line::Blank;
    }
    let msg = match serde_json::from_str::<Value>(text) {
        Ok(v) => v,
        Err(e) => return Line::Reject(format!("bad JSON ({e}); expected one JSON request object per line; closing the connection")),
    };
    let Some(obj) = msg.as_object() else {
        return Line::Reject("not a control request (expected a JSON object); closing the connection".into());
    };
    let Some(method) = obj.get("method").and_then(Value::as_str) else {
        return Line::Reject("not a control request (missing string `method`); closing the connection".into());
    };
    let request_id = match obj.get("requestId") {
        Some(Value::String(s)) => Some(s.clone()),
        Some(Value::Number(n)) => Some(n.to_string()),
        _ => None,
    };
    Line::Request {
        id: obj.get("id").cloned().unwrap_or(Value::Null),
        method: method.to_string(),
        params: obj.get("params").cloned().unwrap_or(json!({})),
        request_id,
    }
}

/// Read one `\n`-terminated line of at most `max` bytes into `buf` (newline stripped).
/// `Ok(false)` at end of stream; `Err` when the line is too long or the read fails.
fn read_bounded_line(reader: &mut impl BufRead, buf: &mut Vec<u8>, max: usize) -> Result<bool, String> {
    buf.clear();
    // `take` bounds the bytes buffered for one line: a peer can't make us allocate more than `max`.
    let limit = u64::try_from(max).unwrap_or(u64::MAX).saturating_add(1);
    let n = reader.take(limit).read_until(b'\n', buf).map_err(|e| e.to_string())?;
    if n == 0 {
        return Ok(false);
    }
    if buf.last() == Some(&b'\n') {
        buf.pop();
        if buf.last() == Some(&b'\r') {
            buf.pop();
        }
        return Ok(true);
    }
    if buf.len() > max {
        return Err(format!("request line longer than {max} bytes; closing the connection"));
    }
    // Last line without a newline, then end of stream.
    Ok(true)
}

/// Serve requests from `read`, answering on `out`, until the peer closes, sends something that is
/// not a request, or `handle` returns `None` (app gone). `handle` gets `(method, params,
/// requestId)` and returns the reply object (the request's `id` is added here).
fn serve_lines<R: Read, W: Write>(read: R, mut out: W, mut handle: impl FnMut(String, Value, Option<String>) -> Option<Value>) {
    let mut reader = BufReader::new(read);
    let mut buf = Vec::new();
    loop {
        let reject = match read_bounded_line(&mut reader, &mut buf, MAX_LINE) {
            Ok(false) => return,
            Err(e) => e,
            Ok(true) => match parse_line(&buf) {
                Line::Blank => continue,
                Line::Reject(e) => e,
                Line::Request { id, method, params, request_id } => {
                    let Some(mut r) = handle(method, params, request_id) else { return };
                    if let Some(o) = r.as_object_mut() {
                        o.insert("id".into(), id);
                    }
                    if write_line(&mut out, &r).is_err() {
                        return;
                    }
                    continue;
                }
            },
        };
        let _ = write_line(&mut out, &json!({"ok": false, "error": reject}));
        return;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(input: &[u8]) -> (Vec<Value>, Vec<String>) {
        let mut out = Vec::new();
        let mut calls = Vec::new();
        serve_lines(input, &mut out, |m, _p, _r| {
            calls.push(m);
            Some(json!({"ok": true, "result": null}))
        });
        let replies = String::from_utf8(out).unwrap().lines().map(|l| serde_json::from_str::<Value>(l).unwrap()).collect();
        (replies, calls)
    }

    #[test]
    fn requests_are_answered_in_order_with_ids() {
        let (r, calls) = run(b"{\"id\":1,\"method\":\"a\"}\n\n{\"id\":2,\"method\":\"b\",\"params\":{}}\r\n{\"method\":\"c\"}");
        assert_eq!(calls, ["a", "b", "c"]);
        assert_eq!(r.len(), 3);
        assert_eq!(r[0]["id"], 1);
        assert_eq!(r[1]["id"], 2);
        assert_eq!(r[2]["id"], Value::Null);
    }

    #[test]
    fn http_request_closes_before_the_body_runs() {
        // A browser's cross-origin `fetch` POST: the request line is rejected and the
        // connection closed, so the JSON body is never executed.
        let http = b"POST / HTTP/1.1\r\nHost: 127.0.0.1:9877\r\nOrigin: https://attacker.example\r\nContent-Type: text/plain\r\n\r\n\
            {\"id\":7,\"method\":\"engine.execute\",\"params\":{\"command\":\"file.new\",\"params\":{}}}\n";
        let (r, calls) = run(http);
        assert!(calls.is_empty(), "nothing may run: {calls:?}");
        assert_eq!(r.len(), 1);
        assert_eq!(r[0]["ok"], false);
        assert!(r[0]["error"].as_str().unwrap().contains("closing the connection"));
    }

    #[test]
    fn non_request_json_closes_the_connection() {
        for junk in [
            &b"[1,2]\n{\"method\":\"a\"}\n"[..],
            b"{\"id\":1}\n{\"method\":\"a\"}\n",
            b"{\"method\":5}\n{\"method\":\"a\"}\n",
            b"\xff\xfe\n{\"method\":\"a\"}\n",
        ] {
            let (r, calls) = run(junk);
            assert!(calls.is_empty(), "{calls:?}");
            assert_eq!(r.len(), 1);
            assert_eq!(r[0]["ok"], false);
        }
    }

    #[test]
    fn a_request_before_junk_still_runs() {
        let (r, calls) = run(b"{\"method\":\"a\"}\nGET / HTTP/1.1\n{\"method\":\"b\"}\n");
        assert_eq!(calls, ["a"]);
        assert_eq!(r.len(), 2);
        assert_eq!(r[1]["ok"], false);
    }

    #[test]
    fn overlong_line_is_refused() {
        // One byte over the limit, then a valid request: neither runs.
        let mut input = vec![b'x'; MAX_LINE + 1];
        input.extend_from_slice(b"\n{\"method\":\"a\"}\n");
        let (r, calls) = run(&input);
        assert!(calls.is_empty(), "{calls:?}");
        assert_eq!(r.len(), 1);
        assert!(r[0]["error"].as_str().unwrap().contains("longer than"));
    }

    #[test]
    fn line_at_the_limit_is_accepted() {
        let mut buf = Vec::new();
        let line = vec![b'a'; 10];
        let mut input = line.clone();
        input.push(b'\n');
        assert_eq!(read_bounded_line(&mut &input[..], &mut buf, 10), Ok(true));
        assert_eq!(buf, line);
        assert!(read_bounded_line(&mut &[b'a'; 11][..], &mut buf, 10).is_err());
    }

    #[test]
    fn tcp_connection_is_closed_after_junk() {
        // End to end over a real loopback socket (the transport the app uses).
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (s, _) = listener.accept().unwrap();
            let read = s.try_clone().unwrap();
            serve_lines(read, s, |_, _, _| Some(json!({"ok": true})));
        });
        let mut c = TcpStream::connect(addr).unwrap();
        c.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        c.write_all(b"{\"id\":7,\"method\":\"x\"}\nhello\n{\"method\":\"y\"}\n").unwrap();
        let mut text = String::new();
        c.read_to_string(&mut text).unwrap(); // returns at EOF: the server closed the connection
        server.join().unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2, "{text}");
        assert!(lines[0].contains("\"id\":7"));
        assert!(lines[1].contains("closing the connection"));
    }

    /// A server on an ephemeral port whose requests go to the returned receiver (the test plays
    /// the UI thread).
    fn server(timeout: Duration) -> (std::net::SocketAddr, Receiver<ControlRequest>, ControlEvents) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = channel::<ControlRequest>();
        let events = ControlEvents::default();
        let shared = Shared { tx, ctx: egui::Context::default(), events: events.clone(), replies: Arc::default(), clients: Arc::default(), timeout };
        std::thread::spawn(move || accept_loop(listener, shared));
        (addr, rx, events)
    }

    fn connect(addr: std::net::SocketAddr) -> (BufReader<TcpStream>, TcpStream) {
        let c = TcpStream::connect(addr).unwrap();
        c.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        (BufReader::new(c.try_clone().unwrap()), c)
    }

    fn call(conn: &mut (BufReader<TcpStream>, TcpStream), line: &str) -> Value {
        conn.1.write_all(format!("{line}\n").as_bytes()).unwrap();
        let mut s = String::new();
        conn.0.read_line(&mut s).unwrap();
        serde_json::from_str(&s).unwrap()
    }

    #[test]
    fn extra_connections_are_refused() {
        let (addr, _rx, _events) = server(REQUEST_TIMEOUT);
        // Idle connections are accepted in order and held open.
        let held: Vec<TcpStream> = (0..16).map(|_| TcpStream::connect(addr).unwrap()).collect();
        let mut extra = TcpStream::connect(addr).unwrap();
        extra.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut line = String::new();
        BufReader::new(&mut extra).read_line(&mut line).unwrap();
        assert!(line.contains("too many control connections"), "{line}");
        drop(held);
    }

    /// Regression: a request that timed out still ran later, so a client's retry ran it twice.
    #[test]
    fn a_timed_out_request_is_never_run() {
        let (addr, rx, _events) = server(Duration::from_millis(100));
        let mut c = connect(addr);
        let r = call(&mut c, r#"{"id":1,"method":"engine.execute","params":{"command":"layer.newText"}}"#);
        assert_eq!(r["ok"], false);
        assert_eq!(r["notRun"], true);
        assert!(r["error"].as_str().unwrap().contains("NOT run"), "{r}");
        // The UI thread gets to it late: the request is cancelled, so it must be skipped.
        let req = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(req.state.is_cancelled());
        assert!(!req.state.start(), "a cancelled request must not start");
    }

    #[test]
    fn a_started_request_is_not_cancelled_by_the_timeout() {
        let (addr, rx, _events) = server(Duration::from_millis(100));
        let ui = std::thread::spawn(move || {
            let req = rx.recv().unwrap();
            assert!(req.state.start());
            req
        });
        let mut c = connect(addr);
        let r = call(&mut c, r#"{"id":1,"method":"slow"}"#);
        let req = ui.join().unwrap();
        assert!(!req.state.is_cancelled());
        assert!(r["error"].as_str().unwrap().contains("may still complete"), "{r}");
        assert!(r.get("notRun").is_none());
    }

    /// A retry with the same `requestId` (here on a new connection, as after a dropped one) gets
    /// the first reply and doesn't run the command again.
    #[test]
    fn a_repeated_request_id_runs_once() {
        let (addr, rx, _events) = server(REQUEST_TIMEOUT);
        let runs = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&runs);
        std::thread::spawn(move || {
            for req in rx {
                let n = counter.fetch_add(1, Ordering::SeqCst) + 1;
                assert!(req.params.get("requestId").is_none(), "requestId must not reach the engine params");
                let _ = req.reply.send(json!({"ok": true, "result": {"layer": n}}));
            }
        });
        let line = r#"{"id":1,"requestId":"agent-7","method":"engine.execute","params":{"command":"layer.newText","params":{}}}"#;
        let first = call(&mut connect(addr), line);
        let again = call(&mut connect(addr), &line.replace("\"id\":1", "\"id\":2"));
        assert_eq!(first["result"]["layer"], 1);
        assert_eq!(again["result"], first["result"]);
        assert_eq!(again["id"], 2);
        assert_eq!(runs.load(Ordering::SeqCst), 1, "the retry must not run again");
        // Without a requestId (or with another one) it runs.
        call(&mut connect(addr), r#"{"id":3,"method":"engine.execute","params":{}}"#);
        call(&mut connect(addr), r#"{"id":4,"requestId":"agent-8","method":"engine.execute","params":{}}"#);
        assert_eq!(runs.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn a_retry_after_a_cancelled_timeout_runs() {
        let mut cache = ReplyCache::default();
        assert!(matches!(cache.begin("a"), Lookup::Miss));
        assert!(matches!(cache.begin("a"), Lookup::Running));
        cache.finish("a", None);
        assert!(matches!(cache.begin("a"), Lookup::Miss));
        cache.finish("a", Some(json!({"ok": true})));
        assert!(matches!(cache.begin("a"), Lookup::Hit(_)));
        // Big replies are not kept.
        assert!(matches!(cache.begin("big"), Lookup::Miss));
        cache.finish("big", Some(json!({"png": "x".repeat(MAX_CACHED_REPLY)})));
        assert!(matches!(cache.begin("big"), Lookup::Miss));
        // Bounded: the oldest ids are forgotten.
        for i in 0..REPLY_CACHE {
            cache.begin(&i.to_string());
        }
        assert_eq!(cache.entries.len(), REPLY_CACHE);
        assert!(matches!(cache.begin("a"), Lookup::Miss));
    }

    #[test]
    fn subscribed_connections_receive_change_events() {
        let (addr, rx, events) = server(REQUEST_TIMEOUT);
        std::thread::spawn(move || {
            for req in rx {
                let _ = req.reply.send(json!({"ok": true, "result": null}));
            }
        });
        let mut c = connect(addr);
        let r = call(&mut c, r#"{"id":1,"method":"events.subscribe"}"#);
        assert_eq!(r["ok"], true, "{r}");
        assert_eq!(r["result"]["subscribed"], true);
        assert_eq!(events.subscribers(), 1);
        // Requests still work on the connection, and events arrive between replies.
        assert_eq!(call(&mut c, r#"{"id":2,"method":"ui.inspect"}"#)["id"], 2);
        events.publish(&json!({"event": "project.changed", "revision": 5}));
        let mut s = String::new();
        c.0.read_line(&mut s).unwrap();
        let ev: Value = serde_json::from_str(&s).unwrap();
        assert_eq!(ev, json!({"event": "project.changed", "revision": 5}));
        // A closed connection is dropped from the subscribers.
        drop(c);
        for _ in 0..100 {
            events.publish(&json!({"event": "project.changed", "revision": 6}));
            if events.subscribers() == 0 {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert_eq!(events.subscribers(), 0);
    }

    /// An agent's claims go when its last connection closes (not while another one is open).
    #[test]
    fn closing_the_last_connection_of_a_client_releases_its_claims() {
        let (addr, rx, _events) = server(REQUEST_TIMEOUT);
        let seen = Arc::new(Mutex::new(Vec::<Value>::new()));
        let log = Arc::clone(&seen);
        std::thread::spawn(move || {
            for req in rx {
                lock(&log).push(json!({"method": req.method, "params": req.params}));
                let _ = req.reply.send(json!({"ok": true, "result": null}));
            }
        });
        let claim = r#"{"id":1,"method":"engine.execute","params":{"command":"comp.claim","params":{"comp":1,"client":"agentA"}}}"#;
        let mut first = connect(addr);
        let mut second = connect(addr);
        call(&mut first, claim);
        call(&mut second, claim);
        let releases = |seen: &Arc<Mutex<Vec<Value>>>| lock(seen).iter().filter(|r| r["params"]["command"] == "comp.release").cloned().collect::<Vec<_>>();
        drop(first);
        // Still in use by the second connection: nothing is released (give the server a moment).
        std::thread::sleep(Duration::from_millis(300));
        assert!(releases(&seen).is_empty());
        drop(second);
        for _ in 0..100 {
            if !releases(&seen).is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let r = releases(&seen);
        assert_eq!(r.len(), 1, "{r:?}");
        assert_eq!(r[0]["params"]["params"], json!({"client": "agentA"}));
    }

    #[test]
    fn a_busy_port_is_reported() {
        let held = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = held.local_addr().unwrap().port();
        let Err(e) = start(port, egui::Context::default(), channel().0) else { panic!("the port is taken") };
        assert!(e.contains(&format!("port {port} is already in use")), "{e}");
        assert!(e.contains("EFFECTCRAFT_CONTROL_PORT"), "{e}");
    }
}
