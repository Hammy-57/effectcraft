//! A render box's link to the app it mirrors (`effectcraft-cli serve --follow PORT`, see
//! [`effectcraft_engine::remote_follow`]).
//!
//! The host thread pulls `app.sync` from the app's control port: before each request it runs
//! (a render sees the edit made just before it), and every [`ACTIVE`] / [`IDLE`] meanwhile, so the
//! box keeps up while nobody asks. An app without `app.sync` (older builds) is followed by saving
//! a copy (`file.saveCopy`) when its revision changed, only before requests: a copy of a big
//! project costs the app's UI thread a moment, which a pull every half second would make a
//! stutter. Nothing here ever fails the host: errors are kept for `host.info` and retried.

use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use effectcraft_engine::Session;
use effectcraft_engine::remote_follow::{Applied, Replica, apply_cursors};
use serde_json::{Value, json};

/// Pull interval while the app's project keeps changing.
pub const ACTIVE: Duration = Duration::from_millis(200);
/// Pull interval when it hasn't changed for [`CALM_AFTER`].
pub const IDLE: Duration = Duration::from_millis(500);
const CALM_AFTER: Duration = Duration::from_secs(10);
/// Retry interval while the app is unreachable.
pub const DOWN: Duration = Duration::from_secs(2);
/// Longest wait for a reply with no project in it.
const SHORT_REPLY: Duration = Duration::from_secs(10);
/// Longest wait for the whole project (serializing a big one on the app's side).
const LONG_REPLY: Duration = Duration::from_secs(120);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    /// The app answers `app.sync`.
    Sync,
    /// An older app: saved copies.
    Copy,
}

pub struct Follower {
    addr: SocketAddr,
    name: String,
    conn: Option<(BufReader<TcpStream>, TcpStream)>,
    next_id: u64,
    mode: Option<Mode>,
    replica: Replica,
    /// The app revision the last saved copy had (copy mode).
    copied: Option<u64>,
    copy_path: PathBuf,
    next_pull: Instant,
    last_change: Instant,
    stats: Stats,
}

#[derive(Default)]
struct Stats {
    pulls: u64,
    patches: u64,
    projects: u64,
    errors: u64,
    bytes: u64,
    last_ms: f64,
    last_error: Option<String>,
    connected: bool,
}

impl Follower {
    /// Follow the app on control port `port` of this machine.
    pub fn new(port: u16) -> Follower {
        let pid = std::process::id();
        let now = Instant::now();
        Follower {
            addr: SocketAddr::from(([127, 0, 0, 1], port)),
            name: format!("render-box-{pid}"),
            conn: None,
            next_id: 1,
            mode: None,
            replica: Replica::default(),
            copied: None,
            copy_path: std::env::temp_dir().join(format!("effectcraft-render-box-{pid}.ecproj")),
            next_pull: now,
            last_change: now,
            stats: Stats::default(),
        }
    }

    /// How long until the next periodic pull.
    pub fn wait(&self) -> Duration {
        self.next_pull.saturating_duration_since(Instant::now())
    }

    /// The periodic pull, when it is due (saved copies only before requests).
    pub fn tick(&mut self, s: &mut Session) {
        if Instant::now() < self.next_pull {
            return;
        }
        if self.mode == Some(Mode::Copy) {
            self.next_pull = Instant::now() + DOWN;
            return;
        }
        let _ = self.pull(s);
    }

    /// Bring the session to the app's latest revision now; `Err` when it may be behind.
    pub fn pull(&mut self, s: &mut Session) -> Result<(), String> {
        let started = Instant::now();
        let r = match self.mode {
            Some(Mode::Copy) => self.pull_copy(s),
            _ => self.pull_sync(s),
        };
        let now = Instant::now();
        self.stats.pulls += 1;
        self.stats.last_ms = now.duration_since(started).as_secs_f64() * 1000.0;
        let interval = match &r {
            Ok(changed) => {
                self.stats.last_error = None;
                if *changed {
                    self.last_change = now;
                }
                if now.duration_since(self.last_change) < CALM_AFTER { ACTIVE } else { IDLE }
            }
            Err(e) => {
                if self.stats.last_error.as_deref() != Some(e.as_str()) {
                    eprintln!("render box: following 127.0.0.1:{}: {e}", self.addr.port());
                }
                self.stats.errors += 1;
                self.stats.last_error = Some(e.clone());
                if self.conn.is_none() { DOWN } else { IDLE }
            }
        };
        self.next_pull = now + interval;
        r.map(|_| ())
    }

    fn pull_sync(&mut self, s: &mut Session) -> Result<bool, String> {
        let have = self.replica.have();
        let timeout = if have.is_some() { SHORT_REPLY } else { LONG_REPLY };
        let reply = match self.call("app.sync", json!({"follower": self.name, "have": have}), timeout) {
            Err(e) if self.mode.is_none() && e.contains("unknown method") => {
                eprintln!("render box: the app has no app.sync (an older build); following it by saved copies before each request");
                self.mode = Some(Mode::Copy);
                return self.pull_copy(s);
            }
            r => r?,
        };
        self.mode = Some(Mode::Sync);
        self.stats.bytes += reply.get("bytes").and_then(Value::as_u64).unwrap_or(0);
        match self.replica.apply(s, &reply)? {
            Applied::Same => Ok(false),
            Applied::Patch => {
                self.stats.patches += 1;
                Ok(true)
            }
            Applied::Project => {
                self.stats.projects += 1;
                Ok(true)
            }
        }
    }

    fn pull_copy(&mut self, s: &mut Session) -> Result<bool, String> {
        let revision = self.call("app.revision", json!({}), SHORT_REPLY)?.get("revision").and_then(Value::as_u64).ok_or("app.revision without `revision`")?;
        let changed = self.copied != Some(revision);
        if changed {
            let path = self.copy_path.to_string_lossy().into_owned();
            self.call("engine.execute", json!({"command": "file.saveCopy", "params": {"path": path}}), LONG_REPLY)?;
            let text = std::fs::read_to_string(&self.copy_path).map_err(|e| format!("{path}: {e}"))?;
            let _ = std::fs::remove_file(&self.copy_path);
            self.stats.bytes += text.len() as u64;
            let p = effectcraft_engine::project::Project::from_json(&text).map_err(|e| format!("the app's saved copy: {e}"))?;
            self.replica.load(s, revision, p);
            self.copied = Some(revision);
            self.stats.projects += 1;
        }
        // The user's comp and time.
        if let Ok(st) = self.call("engine.execute", json!({"command": "editor.state", "params": {}}), SHORT_REPLY) {
            let comp = st.get("active_comp").and_then(Value::as_u64);
            let time = comp.and_then(|c| st.get("times").and_then(|t| t.get(c.to_string())).cloned());
            if let Some(c) = comp {
                apply_cursors(s, &json!({"user": {"comp": c, "time": time}}));
            }
        }
        Ok(changed)
    }

    /// One request on the persistent connection; a failed or late reply drops the connection
    /// (a late reply would answer the next request).
    fn call(&mut self, method: &str, params: Value, timeout: Duration) -> Result<Value, String> {
        let r = self.call_inner(method, params, timeout);
        if r.as_ref().is_err_and(|e| e.starts_with("link: ")) {
            self.conn = None;
            self.stats.connected = false;
            // What the app last sent is unknown: start from the whole project again.
            self.replica.reset();
            self.copied = None;
        }
        r
    }

    fn call_inner(&mut self, method: &str, params: Value, timeout: Duration) -> Result<Value, String> {
        if self.conn.is_none() {
            let stream = TcpStream::connect_timeout(&self.addr, Duration::from_secs(2)).map_err(|e| format!("link: the app is not reachable ({e})"))?;
            stream.set_nodelay(true).ok();
            let read = stream.try_clone().map_err(|e| format!("link: {e}"))?;
            self.conn = Some((BufReader::new(read), stream));
            self.stats.connected = true;
        }
        let Some((reader, writer)) = self.conn.as_mut() else { return Err("link: not connected".into()) };
        let id = self.next_id;
        self.next_id += 1;
        writeln!(writer, "{}", json!({"id": id, "method": method, "params": params})).map_err(|e| format!("link: {e}"))?;
        reader.get_ref().set_read_timeout(Some(timeout)).map_err(|e| format!("link: {e}"))?;
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => return Err("link: the app closed the connection".into()),
            Ok(_) => {}
            Err(e) => return Err(format!("link: no reply to {method} ({e})")),
        }
        let v: Value = serde_json::from_str(&line).map_err(|e| format!("link: bad reply ({e})"))?;
        if v.get("id") != Some(&json!(id)) {
            return Err("link: the reply is for another request".into());
        }
        match v.get("ok").and_then(Value::as_bool) {
            Some(true) => Ok(v.get("result").cloned().unwrap_or(Value::Null)),
            _ => Err(v.get("error").and_then(Value::as_str).unwrap_or("error").to_string()),
        }
    }

    /// For `host.info`: `{app, mode, appRevision, connected, pulls, patches, projects, errors,
    /// bytes, lastMs, lastError}`.
    pub fn info(&self) -> Value {
        let st = &self.stats;
        json!({
            "app": self.addr.port(),
            "mode": match self.mode { Some(Mode::Sync) => "sync", Some(Mode::Copy) => "copy", None => "starting" },
            "appRevision": self.replica.have().or(self.copied),
            "connected": st.connected,
            "pulls": st.pulls, "patches": st.patches, "projects": st.projects, "errors": st.errors,
            "bytes": st.bytes, "lastMs": (st.last_ms * 10.0).round() / 10.0, "lastError": st.last_error,
        })
    }
}
