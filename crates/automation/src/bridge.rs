//! Blocking client for the desktop app's JSON-lines control channel (`docs/control-protocol.md`).
//! One persistent loopback connection, reconnected once if the app went away.
//!
//! [`ControlTransport`] abstracts where control requests go: [`BridgeClient`] (a running app over
//! TCP) or an app embedded in this process, such as `effectcraft-cli mcp --ui`'s offscreen UI
//! (implemented above this crate, which never depends on egui).

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use serde_json::{Value, json};

use crate::{Error, Result};

/// Slightly longer than the app's own 60 s per-request timeout, so its error reply wins.
const READ_TIMEOUT: Duration = Duration::from_secs(65);

/// Something that answers control-channel requests (`docs/control-protocol.md`).
pub trait ControlTransport: Send {
    /// Call a control method; returns the reply's `result`, or the app's `error` as [`Error::App`].
    fn call(&mut self, method: &str, params: Value) -> Result<Value>;
    /// The MCP mode name this transport gives (`initialize`'s server title, `effectcraft-cli info`).
    fn mode(&self) -> &'static str {
        "bridge"
    }
}

/// Unwrap a control reply (`{"ok": true, "result": …}` / `{"ok": false, "error": …}`).
pub fn reply_result(v: Value) -> Result<Value> {
    if v.get("ok").and_then(Value::as_bool) == Some(true) {
        return Ok(v.get("result").cloned().unwrap_or(Value::Null));
    }
    Err(Error::App(v.get("error").and_then(Value::as_str).unwrap_or("error").to_string()))
}

impl ControlTransport for BridgeClient {
    fn call(&mut self, method: &str, params: Value) -> Result<Value> {
        BridgeClient::call(self, method, params)
    }
}

pub struct BridgeClient {
    addr: String,
    conn: Option<(BufReader<TcpStream>, TcpStream)>,
    next_id: u64,
}

impl BridgeClient {
    /// `addr`: a port (`9877`) or `host:port` on loopback (`127.0.0.1`, `localhost`, `[::1]`).
    pub fn new(addr: &str) -> Result<Self> {
        let addr = if addr.parse::<u16>().is_ok() { format!("127.0.0.1:{addr}") } else { addr.to_string() };
        let host = addr.rsplit_once(':').map(|(h, _)| h).unwrap_or(&addr);
        if !matches!(host, "127.0.0.1" | "localhost" | "[::1]" | "::1") {
            return Err(Error::BadArgs(format!("bridge address must be loopback (PORT or 127.0.0.1:PORT), got `{addr}`")));
        }
        Ok(Self { addr, conn: None, next_id: 1 })
    }

    pub fn addr(&self) -> &str {
        &self.addr
    }

    fn connect(&self) -> Result<(BufReader<TcpStream>, TcpStream)> {
        let sa = self
            .addr
            .to_socket_addrs()
            .map_err(|e| Error::Bridge(format!("{}: {e}", self.addr)))?
            .next()
            .ok_or_else(|| Error::Bridge(format!("cannot resolve {}", self.addr)))?;
        let s = TcpStream::connect_timeout(&sa, Duration::from_secs(5))
            .map_err(|e| Error::Bridge(format!("cannot connect to {} ({e}); start the app with `effectcraft --control <port>`", self.addr)))?;
        s.set_read_timeout(Some(READ_TIMEOUT)).ok();
        s.set_nodelay(true).ok();
        let r = s.try_clone().map_err(|e| Error::Bridge(e.to_string()))?;
        Ok((BufReader::new(r), s))
    }

    /// Call a control method; returns the reply's `result`, or the app's `error` as [`Error::App`].
    pub fn call(&mut self, method: &str, params: Value) -> Result<Value> {
        let mut last = Error::Bridge("not attempted".into());
        for attempt in 0..2 {
            if self.conn.is_none() {
                self.conn = Some(self.connect()?);
            }
            let id = self.next_id;
            self.next_id += 1;
            let Some((r, w)) = self.conn.as_mut() else { continue };
            let line = format!("{}\n", json!({"id": id, "method": method, "params": params}));
            // `Ok(Err(_))`: the connection is dead before a reply (safe to resend once);
            // `Err(_)`: a timeout or bad reply (never resent: the app may have run the request).
            let res = (|| -> Result<std::result::Result<Value, Error>> {
                if let Err(e) = w.write_all(line.as_bytes()) {
                    return Ok(Err(Error::Bridge(e.to_string())));
                }
                let mut buf = String::new();
                match r.read_line(&mut buf) {
                    Ok(0) => return Ok(Err(Error::Bridge("connection closed by the app".into()))),
                    Ok(_) => {}
                    Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {
                        return Err(Error::Bridge(format!("no reply to `{method}` within {}s (is the app window running?)", READ_TIMEOUT.as_secs())));
                    }
                    Err(e) => return Ok(Err(Error::Bridge(e.to_string()))),
                }
                serde_json::from_str(&buf).map(Ok).map_err(|e| Error::Bridge(format!("bad reply: {e}")))
            })();
            match res {
                Ok(Ok(v)) => return reply_result(v),
                Ok(Err(e)) => {
                    // Stale connection (app restarted): reconnect and resend once.
                    self.conn = None;
                    last = e;
                    if attempt == 1 {
                        break;
                    }
                }
                Err(e) => {
                    // The stream may still deliver the late reply: start fresh next call.
                    self.conn = None;
                    return Err(e);
                }
            }
        }
        Err(last)
    }
}
