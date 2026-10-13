//! The desktop app's control server: the control channel's wire (`effectcraft_automation::
//! control_wire`: JSON-lines over loopback, bounded lines and connections, junk closes the
//! connection) feeding requests to the UI thread. See `docs/control-protocol.md`.
//!
//! Robustness for agents that retry: a request that times out before the UI thread started it is
//! cancelled (it never runs, and the reply says so), and a request carrying a top-level
//! `requestId` is answered from a small cache when the same id comes again (on any connection),
//! so a retry never runs a command twice. `events.subscribe` makes the connection also receive
//! `{"event": …}` lines when the project or selection changes.
//!
//! Composition claims (`comp.claim`) are released when the last connection that used their
//! `client` closes, so an agent that goes away doesn't keep comps locked until the claims expire.

use std::collections::BTreeSet;
use std::io::Write;
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use effectcraft_automation::control_wire::{self, Clients, ReplyCache, SharedOut, serve_lines, write_line};
use effectcraft_ui_egui::ControlRequest;
use effectcraft_ui_egui::control::ControlEvents;
use serde_json::{Value, json};

/// How long a request may wait for the UI thread.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
/// Change events a connection can receive (`events.subscribe`).
const EVENTS: [&str; 2] = ["project.changed", "selection.changed"];

/// A started control server: requests for the UI thread and the change-event hub it publishes to.
pub struct Started {
    pub requests: Receiver<ControlRequest>,
    pub events: ControlEvents,
    pub port: u16,
}

/// Listen on `127.0.0.1:port`. A port that can't be bound is an error the app shows the user.
pub fn start(port: u16, ctx: egui::Context) -> Result<Started, String> {
    let listener = TcpListener::bind(("127.0.0.1", port)).map_err(|e| bind_error(port, &e))?;
    let port = listener.local_addr().map(|a| a.port()).unwrap_or(port);
    let (tx, rx) = channel::<ControlRequest>();
    log::info!("control server listening on 127.0.0.1:{port}");
    let events = ControlEvents::default();
    let shared = Shared { tx, ctx, events: events.clone(), replies: Arc::default(), clients: Arc::default(), timeout: REQUEST_TIMEOUT };
    std::thread::spawn(move || accept_loop(listener, shared));
    Ok(Started { requests: rx, events, port })
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
    clients: Arc<Clients>,
    timeout: Duration,
}

fn accept_loop(listener: TcpListener, shared: Shared) {
    control_wire::accept_loop(listener, Arc::new(AtomicBool::new(false)), move |stream| serve(stream, &shared));
}

fn serve(stream: TcpStream, shared: &Shared) {
    let Ok(read) = stream.try_clone() else { return };
    let Ok(closer) = stream.try_clone() else { return };
    let out = SharedOut(Arc::new(Mutex::new(stream)));
    let closed = Arc::new(AtomicBool::new(false));
    let mut subscribed = false;
    let mut used = BTreeSet::new();
    let events_out = out.clone();
    let events_closed = Arc::clone(&closed);
    serve_lines(read, out, |method, params, request_id| {
        shared.clients.note(&mut used, &method, &params);
        if method == "events.subscribe" {
            let (rx, revision) = shared.events.subscribe();
            if !subscribed {
                subscribed = true;
                forward_events(rx, events_out.clone(), Arc::clone(&events_closed));
            }
            return Some(json!({"ok": true, "result": {"subscribed": true, "revision": revision, "events": EVENTS}}));
        }
        dispatch(shared, method, params, request_id.as_deref())
    });
    closed.store(true, Ordering::SeqCst);
    let _ = closer.shutdown(std::net::Shutdown::Both);
    release_clients(shared, used);
}

/// A connection closed: release the claims of the clients no other open connection uses.
fn release_clients(shared: &Shared, used: BTreeSet<String>) {
    let gone = shared.clients.closed(used);
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

/// Write change events to a subscribed connection until it closes.
fn forward_events<W: Write + Send + 'static>(rx: Receiver<Value>, mut out: SharedOut<W>, closed: Arc<AtomicBool>) {
    std::thread::spawn(move || {
        while !closed.load(Ordering::SeqCst) {
            match rx.recv_timeout(Duration::from_millis(500)) {
                Ok(event) => {
                    if write_line(&mut out, &event).is_err() {
                        return;
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
    });
}

/// Run one request on the UI thread (or answer a repeated `requestId` from the cache). `None`
/// when the app is gone.
fn dispatch(shared: &Shared, method: String, params: Value, request_id: Option<&str>) -> Option<Value> {
    control_wire::once(&shared.replies, request_id, || {
        let (req, rrx) = ControlRequest::new(method, params);
        let state = req.state.clone();
        shared.tx.send(req).ok()?;
        shared.ctx.request_repaint();
        let secs = shared.timeout.as_secs();
        Some(match rrx.recv_timeout(shared.timeout) {
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
        })
    })
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Write};
    use std::sync::atomic::{AtomicUsize, Ordering};

    use effectcraft_automation::control_wire::{MAX_CONNECTIONS, lock};

    use super::*;

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
        let held: Vec<TcpStream> = (0..MAX_CONNECTIONS).map(|_| TcpStream::connect(addr).unwrap()).collect();
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
        let Err(e) = start(port, egui::Context::default()) else { panic!("the port is taken") };
        assert!(e.contains(&format!("port {port} is already in use")), "{e}");
        assert!(e.contains("EFFECTCRAFT_CONTROL_PORT"), "{e}");
    }
}
