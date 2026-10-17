//! [`ControlTransport`] over the frame channel: control requests to a cloud frame server's host
//! (`effectcraft-cli serve --frames`), through the same token-checked connection (an SSH tunnel
//! in v1) that carries frames. The connection reconnects with backoff on its own
//! ([`FrameClient`]); a request whose connection drops after it was sent is reported as
//! uncertain, never resent (it may have run).

use std::time::{Duration, Instant};

use effectcraft_engine::remote_wire::{Backoff, Down, Event, FrameClient, Up};
use serde_json::Value;

use crate::bridge::{ControlTransport, reply_result};
use crate::{Error, Result};

/// How long a request waits for a connection before failing.
const CONNECT_WAIT: Duration = Duration::from_secs(10);
/// Longer than the host's own per-request timeout, so its error reply wins.
const REPLY_WAIT: Duration = Duration::from_secs(125);

/// Control requests to a frame server's host.
pub struct RemoteControl {
    client: FrameClient,
    addr: String,
    next_id: u64,
    connected: bool,
    refused: Option<String>,
}

impl RemoteControl {
    /// `addr`: `host:port` of the frame channel (through a tunnel: `127.0.0.1:<local port>`).
    pub fn new(addr: &str, token: &str) -> RemoteControl {
        RemoteControl::with_backoff(addr, token, Backoff::default())
    }

    pub fn with_backoff(addr: &str, token: &str, backoff: Backoff) -> RemoteControl {
        let client = FrameClient::connect(addr, token, "control", backoff);
        RemoteControl { client, addr: addr.to_string(), next_id: 1, connected: false, refused: None }
    }

    /// Note a connection event; `Some(why)` when the connection was lost or refused.
    fn note(&mut self, ev: Event) -> Option<(Down, Vec<u8>)> {
        match ev {
            Event::Connected { .. } => self.connected = true,
            Event::Disconnected { .. } => self.connected = false,
            Event::Refused { error } => {
                self.connected = false;
                self.refused = Some(error);
            }
            Event::Down(d, p) => return Some((d, p)),
        }
        None
    }

    fn refused(&self) -> Result<()> {
        match &self.refused {
            Some(e) => Err(Error::Bridge(format!("{} refused the connection: {e}", self.addr))),
            None => Ok(()),
        }
    }
}

impl ControlTransport for RemoteControl {
    fn call(&mut self, method: &str, params: Value) -> Result<Value> {
        self.refused()?;
        let start = Instant::now();
        // Wait for a connection (the link reconnects in the background).
        while !self.client.connected() {
            let left = CONNECT_WAIT.saturating_sub(start.elapsed());
            if left.is_zero() {
                return Err(Error::Bridge(format!("no connection to the frame server at {} (is the SSH tunnel up?)", self.addr)));
            }
            if let Some(ev) = self.client.recv(left.min(Duration::from_millis(100))) {
                self.note(ev);
                self.refused()?;
            }
        }
        let id = self.next_id;
        self.next_id += 1;
        if !self.client.send(Up::Control { id, method: method.to_string(), params }, vec![]) {
            return Err(Error::Bridge(format!("the connection to {} dropped before `{method}` was sent; nothing ran", self.addr)));
        }
        let sent = Instant::now();
        loop {
            let left = REPLY_WAIT.saturating_sub(sent.elapsed());
            if left.is_zero() {
                return Err(Error::Bridge(format!("no reply to `{method}` within {}s; it may have run", REPLY_WAIT.as_secs())));
            }
            let Some(ev) = self.client.recv(left) else { continue };
            let lost = matches!(ev, Event::Disconnected { .. } | Event::Refused { .. } | Event::Connected { .. });
            match self.note(ev) {
                Some((Down::Control { id: got, reply }, _)) if got == id => return reply_result(reply),
                Some(_) => {}
                None if lost => {
                    return Err(Error::Bridge(format!("the connection to {} dropped; `{method}` may have run (inspect before retrying)", self.addr)));
                }
                None => {}
            }
        }
    }

    fn mode(&self) -> &'static str {
        "frames"
    }
}
