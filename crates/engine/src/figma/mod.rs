//! File ▸ Import ▸ Figma (`file.importFigma`): Figma frames, groups, shapes and text as native,
//! editable layers: shape layers with editable paths, rectangles, ellipses and polystars, solid
//! and gradient fills, strokes (width, alignment, dashes, caps, joins); text layers with style
//! runs; frames as precomps (clipping, rounded corners as masks); groups as nulls with their
//! children parented; masks as track mattes; drop / inner shadows as layer styles; layer blur as
//! Gaussian Blur; opacity, blend modes and visibility. Image fills and node types without a native
//! counterpart come in as PNG renders (2×) placed exactly over the node.
//!
//! Sources: node JSON passed in (`payload` / `payloadPath`): what `integrations/figma/serialize.js`
//! produces (in the EffectCraft Transfer plug-in, through the app's Figma bridge, or in the Figma
//! MCP's `use_figma`, no token), or the REST `nodes` reply; or, optionally, a Figma link or file
//! key + node ids fetched with a personal access token through the REST API ([`fetch`]).
//! [`map`] plans the import; [`build`] makes it one undo step.
//!
//! The token comes from `token`, the `FIGMA_TOKEN` environment variable or the one stored with
//! `figma.setToken` (a file next to the settings, readable only by the user). It is never
//! journaled, logged or written into projects.

pub mod build;
pub mod clipboard;
pub mod fetch;
pub mod map;

use std::path::PathBuf;

use serde_json::{Value, json};

use crate::commands::{CommandSpec, always, b_p, bad, f_p, str_p};
use crate::{EngineError, Result, Session};

const CMD: &str = "file.importFigma";
const TOKEN_FILE: &str = "figma-token";

impl Session {
    fn figma_token_path(&self) -> Option<PathBuf> {
        Some(self.config.as_ref()?.dir()?.join(TOKEN_FILE))
    }

    /// The Figma token: the `token` param, `FIGMA_TOKEN`, or the stored one; and where it came
    /// from.
    fn figma_token(&self, p: &Value) -> Option<(String, &'static str)> {
        if let Some(t) = str_p(p, "token").map(str::trim).filter(|t| !t.is_empty()) {
            return Some((t.to_string(), "param"));
        }
        if let Ok(t) = std::env::var("FIGMA_TOKEN")
            && !t.trim().is_empty()
        {
            return Some((t.trim().to_string(), "env"));
        }
        let t = std::fs::read_to_string(self.figma_token_path()?).ok()?;
        let t = t.trim();
        (!t.is_empty()).then(|| (t.to_string(), "settings"))
    }

    /// Where rendered Figma images are written: next to a saved project, else beside the
    /// settings, else the temp folder.
    fn figma_assets_dir(&self, source: &str) -> PathBuf {
        let safe: String = source.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' }).take(64).collect();
        let base = match self.path.as_deref().map(std::path::Path::new) {
            Some(p) => {
                let stem = p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "Project".into());
                p.parent().map(|d| d.join(format!("{stem} Figma Assets"))).unwrap_or_else(|| PathBuf::from(format!("{stem} Figma Assets")))
            }
            None => self.config.as_ref().and_then(|c| c.dir()).map(|d| d.join("Figma Imports")).unwrap_or_else(|| {
                #[cfg(target_arch = "wasm32")]
                {
                    PathBuf::from("/Figma Imports")
                }
                #[cfg(not(target_arch = "wasm32"))]
                {
                    std::env::temp_dir().join("EffectCraft Figma Imports")
                }
            }),
        };
        base.join(if safe.is_empty() { "import".into() } else { safe })
    }
}

fn set_token(s: &mut Session, p: &Value) -> Result<Value> {
    let path = s.figma_token_path().ok_or_else(|| bad("figma.setToken", "this session has no settings folder; use the FIGMA_TOKEN environment variable"))?;
    let token = str_p(p, "token").map(str::trim).unwrap_or("");
    if token.is_empty() {
        let _ = std::fs::remove_file(&path);
        return Ok(json!({"stored": false}));
    }
    if token.len() > 512 || !token.chars().all(|c| c.is_ascii_graphic()) {
        return Err(bad("figma.setToken", "that doesn't look like a Figma personal access token"));
    }
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d).map_err(|e| EngineError::Other(format!("cannot create {}: {e}", d.display())))?;
    }
    write_private(&path, token.as_bytes()).map_err(|e| EngineError::Other(format!("cannot store the token: {e}")))?;
    Ok(json!({"stored": true}))
}

/// Write a secret (the Figma token, the Figma bridge's pairing code) to `path`, readable only by
/// the user: on unix a new file is created with mode 0600 (never briefly readable by others) and
/// an existing one is narrowed to 0600 before the secret goes in.
pub fn write_private(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        o.mode(0o600);
    }
    let mut f = o.open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    f.write_all(bytes)?;
    f.flush()
}

fn status(s: &mut Session, p: &Value) -> Result<Value> {
    let src = s.figma_token(p).map(|(_, src)| src).unwrap_or("none");
    Ok(json!({"token": src, "api": fetch::API}))
}

/// The node JSON and where it came from: `payload`, `payloadPath`, or the REST API.
struct Source {
    nodes: Vec<Value>,
    images: serde_json::Map<String, Value>,
    /// File key (REST) for image renders.
    rest: Option<(String, String, String)>,
    label: String,
}

fn source(s: &mut Session, p: &Value) -> Result<Source> {
    let ids: Vec<String> = match p.get("nodeIds") {
        Some(Value::Array(a)) => {
            a.iter().filter_map(Value::as_str).map(|x| fetch::normalize_id(x).ok_or_else(|| bad(CMD, format!("bad node id `{x}`")))).collect::<Result<_>>()?
        }
        Some(Value::String(x)) => x.split(',').map(|x| fetch::normalize_id(x).ok_or_else(|| bad(CMD, format!("bad node id `{x}`")))).collect::<Result<_>>()?,
        _ => vec![],
    };
    let read_json = |s: &Session, path: &str| -> Result<Value> {
        let bytes = s.services.read_file(path).map_err(|e| bad(CMD, format!("cannot read {path}: {e}")))?;
        serde_json::from_slice::<Value>(&bytes).map_err(|e| bad(CMD, format!("{path}: not JSON ({e})")))
    };
    // `payloadPaths`: the first file is the payload, the others add `fragments` and `images`.
    let mut parts: Vec<Value> = vec![];
    match (p.get("payload"), str_p(p, "payloadPath"), p.get("payloadPaths")) {
        (Some(v), _, _) if !v.is_null() => parts.push(v.clone()),
        (_, Some(path), _) => parts.push(read_json(s, path)?),
        (_, _, Some(Value::Array(a))) if !a.is_empty() => {
            if a.len() > 10_000 {
                return Err(bad(CMD, "too many payload parts"));
            }
            for path in a {
                let path = path.as_str().ok_or_else(|| bad(CMD, "payloadPaths: file paths"))?;
                parts.push(read_json(s, path)?);
            }
        }
        _ => {}
    }
    if !parts.is_empty() {
        let payload = parts.first().cloned().unwrap_or(Value::Null);
        let mut nodes = map::select_nodes(&payload, &ids).map_err(|e| bad(CMD, e))?;
        let mut images = serde_json::Map::new();
        let mut fragments = serde_json::Map::new();
        for part in &parts {
            if let Some(m) = part.get("images").and_then(Value::as_object) {
                images.extend(m.clone());
            }
            if let Some(m) = part.get("fragments").and_then(Value::as_object) {
                fragments.extend(m.clone());
            }
        }
        resolve_refs(&mut nodes, &mut fragments, 0).map_err(|e| bad(CMD, e))?;
        image_bounds(&mut nodes, &images, 0);
        let label = payload.get("fileKey").and_then(Value::as_str).unwrap_or("payload").to_string();
        return Ok(Source { nodes, images, rest: None, label });
    }
    let (key, mut ids) = match (str_p(p, "url"), str_p(p, "fileKey")) {
        (Some(u), _) => {
            let r = fetch::parse_url(u).map_err(|e| bad(CMD, e))?;
            (r.key, if ids.is_empty() { r.ids } else { ids })
        }
        (None, Some(k)) => (k.to_string(), ids),
        _ => return Err(bad(CMD, "give `url` (a Figma link with ?node-id=…), `fileKey` + `nodeIds`, `payload` or `payloadPath`")),
    };
    if ids.is_empty() {
        return Err(bad(CMD, "which nodes? Copy a link to the selection in Figma (Copy link to selection) or pass `nodeIds`"));
    }
    ids.dedup();
    let Some((token, _)) = s.figma_token(p) else {
        return Err(bad(
            CMD,
            "no Figma token: set FIGMA_TOKEN, run figma.setToken {token}, or pass `token` (Figma ▸ Settings ▸ Security ▸ Personal access tokens)",
        ));
    };
    let base = fetch::check_api_base(str_p(p, "apiBase").unwrap_or(fetch::API)).map_err(|e| bad(CMD, e))?;
    let reply = fetch::nodes(&base, &key, &ids, &token).map_err(EngineError::Other)?;
    let nodes = map::select_nodes(&reply, &[]).map_err(|e| bad(CMD, e))?;
    Ok(Source { nodes, images: Default::default(), rest: Some((base, key.clone(), token)), label: key })
}

/// Large selections arrive in parts (the Figma MCP's replies are size-limited): a child too big
/// for its part is a stub `{"$ref": "<node id>"}`, and a later part carries the node in
/// `fragments`. Replace every stub with its fragment (each used once); a missing one is an error.
fn resolve_refs(nodes: &mut [Value], fragments: &mut serde_json::Map<String, Value>, depth: usize) -> std::result::Result<(), String> {
    // Deeper than the mapper goes (it skips those levels with a note): nothing to resolve.
    if depth > 128 {
        return Ok(());
    }
    for n in nodes.iter_mut() {
        if let Some(id) = n.get("$ref").and_then(Value::as_str).map(str::to_string) {
            match fragments.remove(&id) {
                Some(f) if f.get("$ref").is_none() => *n = f,
                _ => return Err(format!("the payload part for node {id} is missing (serialize it with {{subtree: '{id}'}} and add its file)")),
            }
        }
        if let Some(Value::Array(c)) = n.get_mut("children") {
            resolve_refs(c, fragments, depth + 1)?;
        }
    }
    Ok(())
}

/// An image entry's `bounds` says which box its PNG covers: `"render"` (the node's
/// `absoluteRenderBounds`, shadows and strokes included, as Figma's export endpoints and the
/// Figma MCP's `download_assets` render it) or `{x, y, width, height}`. The node's image is placed
/// over that box instead of its bounding box (the default, which `exportAsync` with
/// `useAbsoluteBounds` and the REST `use_absolute_bounds=true` produce). Every node with an image
/// entry is marked with an image paint, so it comes in as that image.
fn image_bounds(nodes: &mut [Value], images: &serde_json::Map<String, Value>, depth: usize) {
    if images.is_empty() || depth > 64 {
        return;
    }
    for n in nodes {
        let id = n.get("id").and_then(Value::as_str).unwrap_or_default().to_string();
        let id = id.as_str();
        // A node the payload supplies an image for comes in as that image, even when the sender
        // didn't mark it with an image paint (older serializers left its fills out).
        if images.contains_key(id)
            && !has_image_fill(n)
            && let Some(o) = n.as_object_mut()
        {
            match o.get_mut("fills") {
                Some(Value::Array(f)) => f.push(serde_json::json!({"type": "IMAGE"})),
                _ => {
                    o.insert("fills".into(), serde_json::json!([{"type": "IMAGE"}]));
                }
            }
        }
        if let Some(b) = images.get(id).and_then(|e| e.get("bounds")) {
            let bounds = match b {
                Value::String(k) if k == "render" => n.get("absoluteRenderBounds").filter(|r| r.is_object()).cloned(),
                Value::Object(o) if ["x", "y", "width", "height"].iter().all(|k| o.get(*k).and_then(Value::as_f64).is_some_and(f64::is_finite)) => {
                    Some(b.clone())
                }
                _ => None,
            };
            if let (Some(bb), Some(o)) = (bounds, n.as_object_mut()) {
                o.insert("absoluteBoundingBox".into(), bb);
            }
        }
        if let Some(Value::Array(c)) = n.get_mut("children") {
            image_bounds(c, images, depth + 1);
        }
    }
}

fn has_image_fill(n: &Value) -> bool {
    n.get("fills").and_then(Value::as_array).is_some_and(|f| {
        f.iter().any(|p| {
            p.get("visible") != Some(&Value::Bool(false)) && matches!(p.get("type").and_then(Value::as_str), Some("IMAGE" | "VIDEO" | "EMOJI" | "PATTERN"))
        })
    })
}

fn import(s: &mut Session, p: &Value) -> Result<Value> {
    let dry = b_p(p, "dryRun").unwrap_or(false);
    let target = match str_p(p, "target").unwrap_or("auto") {
        "newComp" | "new" => "new",
        "activeComp" | "active" | "into" => "active",
        "auto" => "auto",
        o => return Err(bad(CMD, format!("target: newComp|activeComp|auto, not `{o}`"))),
    };
    let active = match p.get("comp") {
        Some(c) if !c.is_null() => Some(s.resolve_comp(Some(c))?),
        _ => s.active_comp_id(),
    };
    let src = source(s, p)?;
    let all_frames =
        src.nodes.iter().all(|n| matches!(n.get("type").and_then(Value::as_str), Some("FRAME" | "COMPONENT" | "INSTANCE" | "COMPONENT_SET" | "SECTION")));
    let into = match (target, active) {
        ("active", None) => return Err(bad(CMD, "target activeComp: no composition is open")),
        ("active", Some(c)) => Some(c),
        ("auto", Some(c)) if !all_frames => Some(c),
        _ => None,
    };
    let opts = map::Options {
        target: match into.and_then(|c| s.project.comp(c)) {
            Some(c) => map::Target::ActiveComp { width: c.width, height: c.height },
            None => map::Target::NewComp,
        },
    };
    let plan = map::plan(&src.nodes, &opts);
    if dry {
        return Ok(plan.summary());
    }
    let scale = f_p(p, "scale").filter(|x| x.is_finite() && *x >= 0.5 && *x <= 4.0).unwrap_or(2.0);
    let settings = build::Settings {
        origin: "Figma",
        undo_label: "Import from Figma",
        frame_rate: f_p(p, "frameRate").filter(|r| r.is_finite() && *r > 0.0 && *r <= 999.0).unwrap_or(30.0),
        duration: f_p(p, "duration").filter(|d| d.is_finite() && *d > 0.0 && *d <= 86_400.0).unwrap_or(10.0),
        into,
    };
    build::run(s, plan, &src, scale, &settings)
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        clipboard::spec(),
        CommandSpec {
            id: CMD,
            label: "Figma...",
            menu: &[],
            shortcut: None,
            alt_shortcuts: &[],
            params: "{url? (Figma link with ?node-id=), fileKey?, nodeIds? [\"1:2\"], payload? (Figma node JSON: the REST nodes reply, serialize.js output, a node or an array), payloadPath?, payloadPaths? [payload, parts with fragments / images…], target?: auto|newComp|activeComp (auto: frames → new comps, else into the active comp), comp?, frameRate? (30), duration? (10 s), scale? (image renders, 2), token? (else FIGMA_TOKEN or figma.setToken), apiBase? (loopback test servers only), dryRun? (plan only)} → {comp, comps, precomps, layers, native, images, rasterized: [{node, name, reason}], notes}",
            enabled: always,
            run: import,
            // Never journaled: the params can hold a token.
            journal: false,
        },
        CommandSpec {
            id: "figma.setToken",
            label: "Set Figma Token",
            menu: &[],
            shortcut: None,
            alt_shortcuts: &[],
            params: "{token} (a Figma personal access token, stored beside the settings readable only by you; empty removes it)",
            enabled: always,
            run: set_token,
            journal: false,
        },
        crate::query!("figma.status", "Figma Status", "{} → {token: param|env|settings|none, api}", status),
    ]
}

#[cfg(test)]
mod tests;
