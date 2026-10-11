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

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use effectcraft_engine::Session;
use serde_json::{Value, json};

use crate::autosave::AutoSave;

/// Connections served at once (ten agents, their tools and the user's).
pub const MAX_CONNECTIONS: usize = 32;
/// Longest request line.
const MAX_LINE: u64 = 4 * 1024 * 1024;
/// Client names one connection may use.
const MAX_CLIENTS_PER_CONNECTION: usize = 16;
/// How often background jobs are polled while some run or a reply waits for one.
const TICK: Duration = Duration::from_millis(50);
/// How long an idle host sleeps between checks (a throttled checkpoint waits at most this long).
const IDLE: Duration = Duration::from_secs(30);

/// How to run the host.
pub struct HostConfig {
    /// Control port (0: any free port; the chosen one is printed and in the host list).
    pub port: u16,
    /// `<config dir>`: the host list and the recovery checkpoints (none: neither).
    pub config_dir: Option<PathBuf>,
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

/// A reply that waits for background jobs.
struct Waiter {
    command: String,
    result: Value,
    jobs: Vec<String>,
    reply: Sender<Value>,
}

/// Serve until `host.release` (or the listener fails). `make` builds the session on the host's
/// own thread; `ready` gets the port once it listens.
pub fn serve(config: HostConfig, make: Box<dyn FnOnce() -> Result<Session, String> + Send>, ready: impl FnOnce(u16)) -> std::io::Result<()> {
    let listener = TcpListener::bind(("127.0.0.1", config.port))?;
    let port = listener.local_addr()?.port();
    listener.set_nonblocking(true)?;
    let stop = Arc::new(AtomicBool::new(false));
    let (tx, rx) = channel::<Msg>();
    let (started_tx, started_rx) = channel::<Result<(), String>>();
    let worker = {
        let stop = Arc::clone(&stop);
        let dir = config.config_dir.clone();
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
            run(session, rx, &stop, port, dir);
            stop.store(true, Ordering::SeqCst);
        })?
    };
    match started_rx.recv() {
        Ok(Ok(())) => {}
        Ok(Err(e)) => return Err(std::io::Error::other(e)),
        Err(_) => return Err(std::io::Error::other("the host stopped while starting")),
    }
    ready(port);
    let open = Arc::new(AtomicUsize::new(0));
    let clients: Arc<Mutex<BTreeMap<String, usize>>> = Arc::default();
    while !stop.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((stream, _)) => {
                let _ = stream.set_nonblocking(false);
                if open.load(Ordering::SeqCst) >= MAX_CONNECTIONS {
                    let mut s = stream;
                    let _ = writeln!(
                        s,
                        "{}",
                        json!({"id": null, "ok": false, "error": format!("the host serves at most {MAX_CONNECTIONS} connections; closing the connection")})
                    );
                    continue;
                }
                open.fetch_add(1, Ordering::SeqCst);
                let (tx, open, clients) = (tx.clone(), Arc::clone(&open), Arc::clone(&clients));
                let spawned = std::thread::Builder::new().name("ec-host-conn".into()).spawn(move || {
                    connection(stream, &tx, &clients);
                    open.fetch_sub(1, Ordering::SeqCst);
                });
                if spawned.is_err() {
                    open.fetch_sub(1, Ordering::SeqCst);
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(100)),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => {
                stop.store(true, Ordering::SeqCst);
                drop(tx);
                let _ = worker.join();
                return Err(e);
            }
        }
    }
    drop(tx);
    let _ = worker.join();
    // Let connection threads write their last replies (`host.release`'s).
    std::thread::sleep(Duration::from_millis(200));
    Ok(())
}

/// The `client` an engine request runs as.
fn request_client(method: &str, params: &Value) -> Option<String> {
    if method != "engine.execute" {
        return None;
    }
    let c = params.get("params")?.get("client")?.as_str()?;
    (!c.is_empty() && c.len() <= effectcraft_engine::agents::MAX_CLIENT_LEN).then(|| c.to_string())
}

fn connection(stream: TcpStream, tx: &Sender<Msg>, clients: &Mutex<BTreeMap<String, usize>>) {
    let Ok(read) = stream.try_clone() else { return };
    let mut out = stream;
    let mut used: Vec<String> = vec![];
    let mut reader = BufReader::new(read);
    loop {
        let mut line = String::new();
        match (&mut reader).take(MAX_LINE).read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let msg: Value = match serde_json::from_str(line) {
            Ok(v @ Value::Object(_)) => v,
            _ => {
                let _ = writeln!(out, "{}", json!({"id": null, "ok": false, "error": "not a JSON request object; closing the connection"}));
                break;
            }
        };
        let id = msg.get("id").cloned().unwrap_or(Value::Null);
        let Some(method) = msg.get("method").and_then(Value::as_str).map(str::to_string) else {
            let _ = writeln!(out, "{}", json!({"id": id, "ok": false, "error": "missing `method`; closing the connection"}));
            break;
        };
        let params = msg.get("params").cloned().filter(|p| !p.is_null()).unwrap_or_else(|| json!({}));
        if let Some(c) = request_client(&method, &params)
            && !used.contains(&c)
            && used.len() < MAX_CLIENTS_PER_CONNECTION
        {
            *clients.lock().unwrap_or_else(PoisonError::into_inner).entry(c.clone()).or_insert(0) += 1;
            used.push(c);
        }
        let (reply_tx, reply_rx) = channel();
        if tx.send(Msg::Request { method, params, reply: reply_tx }).is_err() {
            let _ = writeln!(out, "{}", json!({"id": id, "ok": false, "error": "the host stopped"}));
            break;
        }
        let mut reply = reply_rx.recv().unwrap_or_else(|_| json!({"ok": false, "error": "the host stopped"}));
        reply["id"] = id;
        if writeln!(out, "{reply}").is_err() {
            break;
        }
    }
    let gone: Vec<String> = {
        let mut map = clients.lock().unwrap_or_else(PoisonError::into_inner);
        used.into_iter()
            .filter(|c| match map.get_mut(c) {
                Some(n) if *n > 1 => {
                    *n -= 1;
                    false
                }
                Some(_) => {
                    map.remove(c);
                    true
                }
                None => false,
            })
            .collect()
    };
    if !gone.is_empty() {
        let _ = tx.send(Msg::Closed(gone));
    }
}

/// Commands that wait for their job unless `wait: false` (others only with `wait: true`).
fn waits(command: &str, params: &Value) -> bool {
    let default = matches!(command, "renderQueue.render" | "render.frame" | "render.contactSheet" | "file.importProject");
    params.get("wait").and_then(Value::as_bool).unwrap_or(default)
}

fn ok(v: Value) -> Value {
    json!({"ok": true, "result": v})
}
fn err(e: impl std::fmt::Display) -> Value {
    json!({"ok": false, "error": e.to_string()})
}

fn run(mut s: Session, rx: Receiver<Msg>, stop: &AtomicBool, port: u16, dir: Option<PathBuf>) {
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
    let mut waiters: Vec<Waiter> = vec![];
    while !stop.load(Ordering::SeqCst) {
        let busy = !waiters.is_empty() || !s.jobs().is_empty();
        let msg = match rx.recv_timeout(if busy { TICK } else { IDLE }) {
            Ok(m) => Some(m),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => break,
        };
        let before = s.revision;
        match msg {
            Some(Msg::Request { method, params, reply }) => {
                if method == "host.release" {
                    let r = release(&mut s, &params);
                    let released = r["ok"] == json!(true);
                    let _ = reply.send(r);
                    if released {
                        stop.store(true, Ordering::SeqCst);
                    }
                } else if let Some(w) = handle(&mut s, &method, params, reply) {
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
        s.poll_background();
        let (done, still): (Vec<Waiter>, Vec<Waiter>) = waiters.into_iter().partition(|w| !s.jobs().iter().any(|j| w.jobs.contains(&j.id)));
        waiters = still;
        for w in done {
            let reply = w.reply.clone();
            let _ = reply.send(finish(&mut s, w));
        }
        if let Some(a) = autosave.as_mut()
            && (s.revision != before || a.info()["pending"] == json!(true))
        {
            let _ = a.checkpoint(&mut s, false);
        }
    }
    for w in waiters {
        let _ = w.reply.send(err("the host stopped before the job ended"));
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

fn handle(s: &mut Session, method: &str, p: Value, reply: Sender<Value>) -> Option<Waiter> {
    let r = match method {
        "engine.execute" => {
            let Some(id) = p.get("command").or(p.get("id")).and_then(Value::as_str).map(str::to_string) else {
                let _ = reply.send(err("missing `command`"));
                return None;
            };
            let params = p.get("params").cloned().filter(|v| !v.is_null()).unwrap_or_else(|| json!({}));
            return execute(s, &id, params, reply);
        }
        "render.frame" => return execute(s, "render.frame", p, reply),
        "engine.commands" => s.execute("command.list", json!({"filter": p.get("filter"), "enabledOnly": p.get("enabledOnly")})).map_or_else(err, ok),
        "app.revision" => ok(json!({"revision": s.revision})),
        "host.info" => ok(json!({"pid": std::process::id(), "project": s.path, "dirty": s.is_dirty(), "jobs": s.jobs().len()})),
        m if m.starts_with("ui.") => err(format!("`{m}` needs the desktop app: this host has no window (engine.execute works)")),
        m => err(format!("unknown method `{m}`")),
    };
    let _ = reply.send(r);
    None
}

/// Run an engine command; a waiting one starts its job and replies when it ends.
fn execute(s: &mut Session, id: &str, params: Value, reply: Sender<Value>) -> Option<Waiter> {
    let tasks = |s: &Session| s.jobs().into_iter().map(|j| j.id).filter(|j| j.starts_with("task:")).collect::<Vec<_>>();
    if id == "jobs.wait" {
        let jobs = tasks(s);
        if !jobs.is_empty() {
            return Some(Waiter { command: id.to_string(), result: json!({}), jobs, reply });
        }
    }
    if !waits(id, &params) {
        let _ = reply.send(s.execute_checked(id, params).map_or_else(err, ok));
        return None;
    }
    let before: Vec<String> = s.jobs().into_iter().map(|j| j.id).collect();
    let mut p = params;
    if let Some(o) = p.as_object_mut() {
        o.insert("wait".into(), json!(false));
    }
    match s.execute_checked(id, p) {
        Err(e) => {
            let _ = reply.send(err(e));
            None
        }
        Ok(result) => {
            let jobs: Vec<String> = s.jobs().into_iter().map(|j| j.id).filter(|j| !before.contains(j)).collect();
            if jobs.is_empty() {
                let _ = reply.send(ok(result));
                return None;
            }
            Some(Waiter { command: id.to_string(), result, jobs, reply })
        }
    }
}

/// The reply for a finished wait: what the blocking command returns, or the job's error.
fn finish(s: &mut Session, w: Waiter) -> Value {
    if w.command == "jobs.wait" {
        return ok(json!({"finished": s.job_log}));
    }
    let mut tasks: Vec<&mut effectcraft_engine::jobs::JobRecord> = s.job_log.iter_mut().filter(|r| w.jobs.contains(&r.id)).collect();
    if let Some(r) = tasks.iter().find(|r| r.status != "done") {
        return err(if r.message.is_empty() { r.status.clone() } else { r.message.clone() });
    }
    if let [task] = tasks.as_mut_slice() {
        return ok(std::mem::take(&mut task.result));
    }
    let mut r = w.result;
    if r.get("running").is_some() {
        r["running"] = json!(false);
    }
    if w.command == "renderQueue.render" {
        let ids: Vec<Value> = r["items"].as_array().map(|a| a.iter().map(|i| i["id"].clone()).collect()).unwrap_or_default();
        if let Ok(list) = s.execute("renderQueue.list", json!({})) {
            r["items"] = json!(list["items"].as_array().map(|a| a.iter().filter(|i| ids.contains(&i["id"])).cloned().collect::<Vec<_>>()).unwrap_or_default());
        }
    }
    let prefix = w.command.split('.').next().unwrap_or("");
    if matches!(prefix, "warp" | "camera" | "roto")
        && r.get("layer").is_some()
        && let Ok(st) = s.execute(&format!("{prefix}.status"), json!({"layer": r["layer"], "effect": r["effect"]}))
    {
        r["status"] = st;
    }
    if r.get("progress").is_some() {
        r["progress"] = Value::Null;
    }
    ok(r)
}

/// Talk to a host (or the desktop app) once: send `method` with `params`, return its reply.
pub fn request(port: u16, method: &str, params: Value) -> std::io::Result<Value> {
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let stream = TcpStream::connect_timeout(&addr, Duration::from_secs(2))?;
    stream.set_read_timeout(Some(Duration::from_secs(120)))?;
    let mut w = stream.try_clone()?;
    writeln!(w, "{}", json!({"id": 1, "method": method, "params": params}))?;
    let mut line = String::new();
    BufReader::new(stream).take(MAX_LINE).read_line(&mut line)?;
    serde_json::from_str(&line).map_err(std::io::Error::other)
}
