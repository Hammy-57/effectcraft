//! The Motion Library in the browser: the Worker in front of the library bucket, read with the
//! page's `fetch` (the desktop app uses its own HTTPS client). The token comes from the settings
//! (`library.configure`, kept in browser storage). Downloads are cached in memory for the page.

use std::sync::Arc;

use effectcraft_engine::motionlib::{Done, MemStore, Remote, valid_key};
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;

struct FetchRemote {
    base: String,
    token: String,
}

async fn fetch(url: String, token: String) -> Result<Vec<u8>, String> {
    let js = |e: wasm_bindgen::JsValue| format!("network: {}", e.as_string().unwrap_or_else(|| format!("{e:?}")));
    let init = web_sys::RequestInit::new();
    init.set_method("GET");
    let headers = web_sys::Headers::new().map_err(js)?;
    if !token.is_empty() {
        headers.set("Authorization", &format!("Bearer {token}")).map_err(js)?;
    }
    init.set_headers(&headers);
    let req = web_sys::Request::new_with_str_and_init(&url, &init).map_err(js)?;
    let window = web_sys::window().ok_or("no window")?;
    let resp: web_sys::Response = JsFuture::from(window.fetch_with_request(&req)).await.map_err(js)?.dyn_into().map_err(js)?;
    match resp.status() {
        200 => {}
        404 => return Err("not found".into()),
        401 | 403 => return Err("the library refused the token (library.configure)".into()),
        code => return Err(format!("the library answered HTTP {code}")),
    }
    let buf = JsFuture::from(resp.array_buffer().map_err(js)?).await.map_err(js)?;
    Ok(js_sys::Uint8Array::new(&buf).to_vec())
}

impl Remote for FetchRemote {
    fn get(&self, key: &str, done: Done) {
        if !valid_key(key) {
            return done(Err(format!("bad key `{key}`")));
        }
        let (url, token, key) = (format!("{}/{key}", self.base), self.token.clone(), key.to_string());
        wasm_bindgen_futures::spawn_local(async move {
            done(fetch(url, token).await.map_err(|e| if e == "not found" { format!("not found: {key}") } else { format!("{key}: {e}") }));
        });
    }
    fn describe(&self) -> String {
        self.base.clone()
    }
}

/// Wire the session's Motion Library to the page's fetch.
pub fn install(handle: &effectcraft_engine::motion_library::Handle) {
    handle.set_transport(
        Arc::new(|base: &str, token: &str| {
            Ok(Arc::new(FetchRemote { base: base.trim().trim_end_matches('/').to_string(), token: token.trim().to_string() }) as Arc<dyn Remote>)
        }),
        Arc::new(MemStore::default()),
    );
}
