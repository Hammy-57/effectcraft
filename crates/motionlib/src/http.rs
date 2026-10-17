//! The Worker in front of the bucket, over HTTPS (desktop and Linux).
//!
//! TLS is rustls with the RustCrypto provider (pure Rust, no `ring` or `aws-lc` C code) and the
//! operating system's certificate verification, as FilmCraft's model downloads. Requests carry
//! `Authorization: Bearer <token>`; the token never appears in messages.

use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::remote::{Done, Remote};
use crate::store::valid_key;

/// Downloads at once (thumbnails scroll into view in bursts).
const WORKERS: usize = 4;
/// Largest download accepted (the biggest blobs are previews and images under the publish limit).
const MAX_BODY: u64 = 1 << 30;

type Job = Box<dyn FnOnce() + Send>;

pub struct HttpRemote {
    base: String,
    token: String,
    agent: ureq::Agent,
    queue: Mutex<Option<Sender<Job>>>,
}

fn agent() -> ureq::Agent {
    let provider = Arc::new(rustls_rustcrypto::provider());
    ureq::Agent::config_builder()
        .tls_config(
            ureq::tls::TlsConfig::builder()
                .provider(ureq::tls::TlsProvider::Rustls)
                .root_certs(ureq::tls::RootCerts::PlatformVerifier)
                .unversioned_rustls_crypto_provider(provider)
                .build(),
        )
        .timeout_connect(Some(Duration::from_secs(15)))
        .timeout_global(Some(Duration::from_secs(600)))
        .build()
        .into()
}

fn describe_error(e: ureq::Error) -> String {
    match e {
        ureq::Error::StatusCode(404) => "not found".into(),
        ureq::Error::StatusCode(401 | 403) => "the library refused the token (check EFFECTCRAFT_LIBRARY_TOKEN or library.configure)".into(),
        ureq::Error::StatusCode(code) => format!("the library answered HTTP {code}"),
        other => format!("network: {other}"),
    }
}

impl HttpRemote {
    pub fn new(base: &str, token: &str) -> Result<HttpRemote, String> {
        let base = base.trim().trim_end_matches('/').to_string();
        if !(base.starts_with("https://") || base.starts_with("http://")) {
            return Err(format!("not an http(s) URL: {base}"));
        }
        Ok(HttpRemote { base, token: token.trim().to_string(), agent: agent(), queue: Mutex::new(None) })
    }

    fn url(&self, key: &str) -> String {
        format!("{}/{key}", self.base)
    }

    fn fetch(agent: &ureq::Agent, url: &str, token: &str) -> Result<Vec<u8>, String> {
        let mut req = agent.get(url);
        if !token.is_empty() {
            req = req.header("Authorization", format!("Bearer {token}"));
        }
        let mut resp = req.call().map_err(describe_error)?;
        resp.body_mut().with_config().limit(MAX_BODY).read_to_vec().map_err(|e| format!("network: {e}"))
    }

    /// Queue `job` on the download workers (started on first use).
    fn spawn(&self, job: Job) -> Result<(), Job> {
        let Ok(mut q) = self.queue.lock() else { return Err(job) };
        if q.is_none() {
            let (tx, rx) = channel::<Job>();
            let rx = Arc::new(Mutex::new(rx));
            for i in 0..WORKERS {
                let rx: Arc<Mutex<Receiver<Job>>> = rx.clone();
                let _ = std::thread::Builder::new().name(format!("ec-library-{i}")).spawn(move || {
                    loop {
                        let next = match rx.lock() {
                            Ok(r) => r.recv(),
                            Err(_) => return,
                        };
                        match next {
                            Ok(job) => job(),
                            Err(_) => return,
                        }
                    }
                });
            }
            *q = Some(tx);
        }
        match q.as_ref() {
            Some(tx) => tx.send(job).map_err(|e| e.0),
            None => Err(job),
        }
    }
}

impl Remote for HttpRemote {
    fn get(&self, key: &str, done: Done) {
        if !valid_key(key) {
            return done(Err(format!("bad key `{key}`")));
        }
        let (agent, url, token, key) = (self.agent.clone(), self.url(key), self.token.clone(), key.to_string());
        let job: Job = Box::new(move || {
            done(Self::fetch(&agent, &url, &token).map_err(|e| if e == "not found" { format!("not found: {key}") } else { format!("{key}: {e}") }))
        });
        // No worker thread could start: download on this thread rather than never.
        if let Err(job) = self.spawn(job) {
            job();
        }
    }

    fn put(&self, key: &str, data: &[u8]) -> Result<(), String> {
        if !valid_key(key) {
            return Err(format!("bad key `{key}`"));
        }
        let mut req = self.agent.put(&self.url(key)).header("Content-Type", "application/octet-stream");
        if !self.token.is_empty() {
            req = req.header("Authorization", format!("Bearer {}", self.token));
        }
        req.send(data).map(|_| ()).map_err(|e| format!("upload {key}: {}", describe_error(e)))
    }

    fn describe(&self) -> String {
        self.base.clone()
    }
}
