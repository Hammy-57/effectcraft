//! A windowless Host: one project in memory that several agents work on at once, over the desktop
//! app's JSON-lines control channel (`docs/control-protocol.md`), with no window.
//! `effectcraft-cli serve [--project F] [--control PORT]`; agents join with
//! `effectcraft-cli mcp --bridge PORT` (or the MCP `hosts {join}` tool).
//!
//! One thread owns the session and runs requests one at a time; connection threads only read and
//! write lines. Engine requests run as their `client` (its own cursor; `comp.claim` keeps other
//! agents off a comp), and a client's claims are released when its last connection closes.
//! Commands that wait for a background job (renders, analyses, `jobs.wait`) start the job and
//! reply when it ends, so other agents' requests run meanwhile. Unsaved work is checkpointed
//! (throttled for big projects, see `autosave`), the host is in the host list while it runs, and
//! `host.release` saves the project and stops the host (a handover: the desktop app opens it).
//!
//! A render box (`HostConfig::follow`, `serve --follow PORT`) mirrors the desktop app on that
//! control port instead ([`crate::follow`]): it pulls the app's edits before each request and
//! twice a second meanwhile, refuses edits of its own (the app is the one source of truth), runs
//! at most [`MAX_BOX_RENDERS`] renders at once, and keeps no checkpoints and no host-list entry.

use std::collections::{BTreeSet, VecDeque};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use effectcraft_engine::Session;
use effectcraft_engine::job_waiter::{JobWaiter, Started};
use serde_json::{Value, json};

use crate::autosave::AutoSave;
use crate::control_wire::{self, Clients, ReplyCache, serve_lines};
use crate::follow::Follower;

/// How often background jobs are polled while some run or a reply waits for one.
const TICK: Duration = Duration::from_millis(50);
/// How long an idle host sleeps between checks (a throttled checkpoint waits at most this long).
const IDLE: Duration = Duration::from_secs(30);
/// Renders (and other waiting commands) a render box runs at once; later ones queue.
pub const MAX_BOX_RENDERS: usize = 2;

/// How to run the host.
pub struct HostConfig {
    /// Control port (0: any free port; the chosen one is printed and in the host list).
    pub port: u16,
    /// `<config dir>`: the host list and the recovery checkpoints (none: neither).
    pub config_dir: Option<PathBuf>,
    /// A render box: mirror the desktop app (or another host) on this control port.
    pub follow: Option<u16>,
}

enum Msg {
    Request {
        method: String,
        params: Value,
        reply: Sender<Value>,
    },
    /// A connection closed: release the claims of the clients no other open connection uses.
    Closed(Vec<String>),
}

/// Serve until `host.release` (or the listener fails). `make` builds the session on the host's
/// own thread; `ready` gets the port once it listens.
pub fn serve(config: HostConfig, make: Box<dyn FnOnce() -> Result<Session, String> + Send>, ready: impl FnOnce(u16)) -> std::io::Result<()> {
    let listener = TcpListener::bind(("127.0.0.1", config.port))?;
    let port = listener.local_addr()?.port();
    let stop = Arc::new(AtomicBool::new(false));
    let (tx, rx) = channel::<Msg>();
    let (started_tx, started_rx) = channel::<Result<(), String>>();
    let worker = {
        let stop = Arc::clone(&stop);
        let follow = config.follow;
        // A render box keeps no checkpoints and stays out of the host list.
        let dir = config.config_dir.clone().filter(|_| follow.is_none());
        std::thread::Builder::new().name("ec-host".into()).spawn(move || {
            let session = match make() {
                Ok(s) => s,
                Err(e) => {
                    let _ = started_tx.send(Err(e));
                    stop.store(true, Ordering::SeqCst);
                    return;
                }
            };
            let _ = started_tx.send(Ok(()));
            run(session, rx, &stop, port, dir, follow.map(Follower::new));
            stop.store(true, Ordering::SeqCst);
            control_wire::wake(port);
        })?
    };
    match started_rx.recv() {
        Ok(Ok(())) => {}
        Ok(Err(e)) => return Err(std::io::Error::other(e)),
        Err(_) => return Err(std::io::Error::other("the host stopped while starting")),
    }
    ready(port);
    let clients: Arc<Clients> = Arc::default();
    let replies: Arc<Mutex<ReplyCache>> = Arc::default();
    control_wire::accept_loop(listener, Arc::clone(&stop), move |stream| connection(stream, &tx, &clients, &replies));
    let _ = worker.join();
    // Let connection threads write their last replies (`host.release`'s).
    std::thread::sleep(Duration::from_millis(200));
    Ok(())
}

fn connection(stream: TcpStream, tx: &Sender<Msg>, clients: &Clients, replies: &Mutex<ReplyCache>) {
    let Ok(read) = stream.try_clone() else { return };
    let mut used = BTreeSet::new();
    serve_lines(read, stream, |method, params, request_id| {
        clients.note(&mut used, &method, &params);
        control_wire::once(replies, request_id.as_deref(), || {
            let (reply_tx, reply_rx) = channel();
            if tx.send(Msg::Request { method, params, reply: reply_tx }).is_err() {
                return Some((err(STOPPED), false));
            }
            Some((reply_rx.recv().unwrap_or_else(|_| err(STOPPED)), true))
        })
    });
    let gone = clients.closed(used);
    if !gone.is_empty() {
        let _ = tx.send(Msg::Closed(gone));
    }
}

fn ok(v: Value) -> Value {
    json!({"ok": true, "result": v})
}
fn err(e: impl std::fmt::Display) -> Value {
    json!({"ok": false, "error": e.to_string()})
}

fn run(mut s: Session, rx: Receiver<Msg>, stop: &AtomicBool, port: u16, dir: Option<PathBuf>, mut follow: Option<Follower>) {
    if let Some(d) = &dir {
        effectcraft_engine::hosts::announce(d, "host", Some(port));
        if let Some(p) = s.path.clone() {
            effectcraft_engine::hosts::set_project(Some(&p));
        }
    }
    let mut autosave = dir.as_ref().and_then(|d| AutoSave::new(d, &s.prefs).ok());
    if let Some(a) = &autosave {
        s.autosave.background = false;
        s.autosave_folder_override = Some(a.folder().to_path_buf());
    }
    let mut waiters: Vec<(JobWaiter, Sender<Value>)> = vec![];
    // A render box's renders beyond MAX_BOX_RENDERS, in arrival order.
    let mut queued: VecDeque<Msg> = VecDeque::new();
    if let Some(f) = follow.as_mut() {
        let _ = f.pull(&mut s);
    }
    while !stop.load(Ordering::SeqCst) {
        let busy = !waiters.is_empty() || !s.jobs().is_empty();
        let mut wait = if busy { TICK } else { IDLE };
        if let Some(f) = &follow {
            wait = wait.min(f.wait());
        }
        let msg = if follow.is_some() && waiters.len() < MAX_BOX_RENDERS && !queued.is_empty() {
            queued.pop_front()
        } else {
            match rx.recv_timeout(wait) {
                Ok(m) => Some(m),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => break,
            }
        };
        let msg = match msg {
            Some(Msg::Request { method, params, reply }) if follow.is_some() && waiters.len() >= MAX_BOX_RENDERS && waits(&method, &params) => {
                queued.push_back(Msg::Request { method, params, reply });
                None
            }
            m => m,
        };
        let before = s.revision;
        match msg {
            Some(Msg::Request { method, params, reply }) => {
                let pulled = match follow.as_mut() {
                    // What the agent edited in the app a moment ago is what it renders here.
                    Some(f) if matches!(method.as_str(), "engine.execute" | "render.frame" | "app.revision" | "app.sync") => f.pull(&mut s),
                    _ => Ok(()),
                };
                if let Err(e) = pulled.as_ref()
                    && waits(&method, &params)
                {
                    // Never a render of an older project than the app's.
                    let _ = reply.send(err(format!("{OUT_OF_SYNC}: {e}")));
                } else if method == "host.info"
                    && let Some(f) = &follow
                {
                    let mut info = host_info(&s);
                    info["follow"] = f.info();
                    info["queued"] = json!(queued.len());
                    let _ = reply.send(ok(info));
                } else if follow.is_some() && method == "host.release" {
                    // Nothing to save: the app holds the project.
                    let _ = reply.send(ok(json!({"released": true, "path": null})));
                    stop.store(true, Ordering::SeqCst);
                } else if method == "host.release" {
                    let r = release(&mut s, &params);
                    let released = r["ok"] == json!(true);
                    let _ = reply.send(r);
                    if released {
                        stop.store(true, Ordering::SeqCst);
                    }
                } else if let Some(w) = handle(&mut s, &method, params, reply, follow.is_some()) {
                    waiters.push(w);
                }
            }
            Some(Msg::Closed(clients)) => {
                for c in clients {
                    let _ = s.execute("comp.release", json!({"client": c}));
                }
            }
            None => {}
        }
        if let Some(f) = follow.as_mut() {
            f.tick(&mut s);
        }
        s.poll_background();
        let (done, still): (Vec<_>, Vec<_>) = waiters.into_iter().partition(|(w, _)| !w.pending(&s));
        waiters = still;
        for (w, reply) in done {
            let _ = reply.send(w.finish(&mut s));
        }
        if let Some(a) = autosave.as_mut()
            && (s.revision != before || a.info()["pending"] == json!(true))
        {
            let _ = a.checkpoint(&mut s, false);
        }
    }
    for (_, reply) in waiters {
        let _ = reply.send(err("the host stopped before the job ended"));
    }
    for m in queued {
        if let Msg::Request { reply, .. } = m {
            let _ = reply.send(err("the host stopped before the render started"));
        }
    }
    if let Some(a) = autosave.as_mut() {
        let _ = a.checkpoint(&mut s, true);
    }
    effectcraft_engine::hosts::withdraw();
}

/// `host.release {path?}`: save the project (to `path`, else where it was opened) and stop.
fn release(s: &mut Session, p: &Value) -> Value {
    let saved = match p.get("path").and_then(Value::as_str) {
        Some(path) => s.execute("file.saveAs", json!({"path": path})),
        None if s.path.is_some() => s.execute("file.save", json!({})),
        None if !s.is_dirty() => Ok(json!({})),
        None => return err("the project was never saved: pass `path`"),
    };
    match saved {
        Ok(_) => ok(json!({"released": true, "path": s.path})),
        Err(e) => err(e),
    }
}

/// Whether a request starts a job its reply waits for (a render), which a render box queues.
fn waits(method: &str, p: &Value) -> bool {
    match method {
        "render.frame" => true,
        "engine.execute" => p.get("command").or(p.get("id")).and_then(Value::as_str).is_some_and(|c| c.starts_with("render.") || c == "jobs.wait"),
        _ => false,
    }
}

fn host_info(s: &Session) -> Value {
    json!({"pid": std::process::id(), "project": s.path, "dirty": s.is_dirty(), "jobs": s.jobs().len()})
}

/// What a host that is stopping answers the requests still arriving.
pub const STOPPED: &str = "the host stopped";

/// How a render box's refusal to render starts when it could not reach the app's latest
/// revision (the MCP's render route then renders in the app).
pub const OUT_OF_SYNC: &str = "render box out of sync with the app";

/// What a render box answers a command that changed its project (which it has undone).
pub const READ_ONLY: &str =
    "this is a render box: it mirrors the app's project and is read-only, so the change was undone; make edits in the app (renders, lint and reads work here)";

fn handle(s: &mut Session, method: &str, p: Value, reply: Sender<Value>, read_only: bool) -> Option<(JobWaiter, Sender<Value>)> {
    let r = match method {
        "engine.execute" => {
            let Some(id) = p.get("command").or(p.get("id")).and_then(Value::as_str).map(str::to_string) else {
                let _ = reply.send(err("missing `command`"));
                return None;
            };
            let params = p.get("params").cloned().filter(|v| !v.is_null()).unwrap_or_else(|| json!({}));
            if read_only {
                return execute_read_only(s, &id, params, reply);
            }
            return execute(s, &id, params, reply);
        }
        "render.frame" => return execute(s, "render.frame", p, reply),
        "app.sync" => effectcraft_engine::remote_follow::pull(s, &p).map(|pull| pull.into_reply()).map_or_else(err, ok),
        "engine.commands" => s.execute("command.list", json!({"filter": p.get("filter"), "enabledOnly": p.get("enabledOnly")})).map_or_else(err, ok),
        "app.revision" => ok(json!({"revision": s.revision})),
        "host.info" => ok(host_info(s)),
        m if m.starts_with("ui.") => err(format!("`{m}` needs the desktop app: this host has no window (engine.execute works)")),
        m => err(format!("unknown method `{m}`")),
    };
    let _ = reply.send(r);
    None
}

/// Run an engine command; a waiting one starts its job and replies when it ends.
fn execute(s: &mut Session, id: &str, params: Value, reply: Sender<Value>) -> Option<(JobWaiter, Sender<Value>)> {
    match effectcraft_engine::job_waiter::start(s, id, params) {
        Ok(Started::Waiting(w)) => return Some((w, reply)),
        Ok(Started::Done(v)) => {
            let _ = reply.send(ok(v));
        }
        Err(e) => {
            let _ = reply.send(err(e));
        }
    }
    None
}

/// [`execute`] on a render box: a command that changed the project is undone and refused.
fn execute_read_only(s: &mut Session, id: &str, params: Value, reply: Sender<Value>) -> Option<(JobWaiter, Sender<Value>)> {
    let before = Arc::clone(&s.project);
    let (tx, rx) = channel();
    let waiter = execute(s, id, params, tx);
    if !Arc::ptr_eq(&before, &s.project) {
        s.project = before;
        s.bump();
        let _ = reply.send(err(READ_ONLY));
        return waiter.map(|(w, _)| (w, channel().0));
    }
    match waiter {
        Some((w, _)) => Some((w, reply)),
        None => {
            let _ = reply.send(rx.recv().unwrap_or_else(|_| err("the command sent no reply")));
            None
        }
    }
}

/// Talk to a host (or the desktop app) once: send `method` with `params`, return its reply.
pub fn request(port: u16, method: &str, params: Value) -> std::io::Result<Value> {
    use std::io::{BufRead, BufReader, Read, Write};
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let stream = TcpStream::connect_timeout(&addr, Duration::from_secs(2))?;
    stream.set_read_timeout(Some(Duration::from_secs(120)))?;
    let mut w = stream.try_clone()?;
    writeln!(w, "{}", json!({"id": 1, "method": method, "params": params}))?;
    let mut line = String::new();
    BufReader::new(stream).take(u64::try_from(control_wire::MAX_LINE).unwrap_or(u64::MAX)).read_line(&mut line)?;
    serde_json::from_str(&line).map_err(std::io::Error::other)
}
