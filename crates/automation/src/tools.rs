//! The tool catalogue: names, descriptions, JSON input schemas and implementations. Shared by the
//! MCP server (`tools/list`, `tools/call`) and the CLI's one-shot subcommands, so both behave the
//! same. Every tool maps onto engine commands (`Session::execute`) or control-channel methods.

use serde_json::{Map, Value, json};

use crate::backend::NEED_BRIDGE;
use crate::{Backend, Error, Result, encode_png, recompress_png};

/// A tool's output.
pub enum Reply {
    Json(Value),
    /// A PNG plus a JSON description of it (comp, time, size, path).
    Image {
        png: Vec<u8>,
        info: Value,
    },
}

pub struct ToolDef {
    pub name: &'static str,
    pub description: &'static str,
    /// Only listed (and usable) with a UI: bridged to the running app, or `mcp --ui` (offscreen).
    pub bridge_only: bool,
    schema: fn() -> Value,
    run: fn(&mut Backend, &Value) -> Result<Reply>,
}

impl ToolDef {
    /// The JSON Schema of the tool's arguments.
    pub fn input_schema(&self) -> Value {
        (self.schema)()
    }
    /// The MCP `Tool` object, with a title and annotations (docs/mcp.md).
    pub fn descriptor(&self) -> Value {
        let title = title_of(self.name);
        let read_only = READ_ONLY.contains(&self.name);
        let files = WRITES_FILES.contains(&self.name);
        json!({
            "name": self.name,
            "title": title,
            "description": self.description,
            "inputSchema": self.input_schema(),
            "annotations": {
                "title": title,
                "readOnlyHint": read_only,
                "destructiveHint": !read_only && !files,
                "idempotentHint": read_only || files,
                "openWorldHint": false,
            },
        })
    }

    /// The first argument this tool doesn't take, as a message naming the accepted ones.
    pub fn unknown_arg(&self, args: &Value) -> Option<String> {
        let Some(m) = args.as_object() else { return Some("arguments must be an object".into()) };
        let schema = self.input_schema();
        let props = schema.get("properties")?.as_object()?;
        // `client` (who the agent is) goes with every tool: see `server::guarded_run`.
        let bad = m.keys().find(|k| k.as_str() != "client" && !props.contains_key(*k))?;
        let accepted: Vec<&str> = props.keys().map(String::as_str).collect();
        // `command` ↔ `id` is the usual mix-up between the command tools; else the closest name.
        let hint = match bad.as_str() {
            "command" if props.contains_key("id") => "; did you mean `id`?".to_string(),
            "id" if props.contains_key("command") => "; did you mean `command`?".to_string(),
            _ => effectcraft_engine::suggest::did_you_mean(bad, accepted.iter().copied()),
        };
        Some(format!("unknown argument \"{bad}\" for {}{hint}; expected: {}", self.name, accepted.join(", ")))
    }
}

/// Tools that only read.
const READ_ONLY: &[&str] = &[
    "command_list",
    "list_commands",
    "describe_command",
    "doc_inspect",
    "get_project",
    "get_comp",
    "get_layer",
    "get_property",
    "list_effects",
    "list_fonts",
    "get_state",
    "ui_inspect",
    "ui_elements",
    "get_layout",
    "lint_frame",
    "get_motion",
];
/// Tools that write files (or keep session-side frames) without changing the project.
const WRITES_FILES: &[&str] =
    &["save_project", "render_frame", "render_preview", "render_frames", "screenshot", "render_snapshot", "render_compare", "analyze_audio"];

/// Older tool names kept callable but left out of `tools/list` (`mcp --all-tools` lists them):
/// (alias, canonical tool). The canonical one takes every argument the alias does, except
/// `execute_command`'s `command` key (`command_run` takes `id`).
pub const HIDDEN_ALIASES: &[(&str, &str)] =
    &[("list_commands", "command_list"), ("execute_command", "command_run"), ("get_project", "doc_inspect"), ("render_frame", "render_preview")];

/// The canonical tool a hidden alias stands for.
pub fn alias_of(name: &str) -> Option<&'static str> {
    HIDDEN_ALIASES.iter().find(|(alias, _)| *alias == name).map(|(_, canonical)| *canonical)
}

/// Whether a tool can change the project (anything but queries and file writers).
pub fn may_edit(name: &str) -> bool {
    !READ_ONLY.contains(&name) && !WRITES_FILES.contains(&name)
}

/// `get_project` → "Get project".
fn title_of(name: &str) -> String {
    let s = name.replace('_', " ");
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
}

/// All tools (headless + bridge-only).
pub fn catalogue() -> &'static [ToolDef] {
    TOOLS
}

pub fn find(name: &str) -> Option<&'static ToolDef> {
    TOOLS.iter().find(|t| t.name == name)
}

/// The tools available on a backend (UI tools are hidden in headless mode without a UI).
pub fn available(bridge: bool) -> impl Iterator<Item = &'static ToolDef> {
    TOOLS.iter().filter(move |t| bridge || !t.bridge_only)
}

/// The JSON Schema of a tool's `structuredContent` (MCP 2025-06-18 `outputSchema`), for tools
/// whose result shape is stable. Clients that negotiated 2025-06-18+ get every JSON result as
/// `structuredContent` too ([`structured`]); only these declare its shape.
pub fn output_schema(name: &str) -> Option<Value> {
    let object = |props: Value, required: &[&str]| json!({"type": "object", "properties": props, "required": required});
    let int = json!({"type": "integer"});
    Some(match name {
        "get_state" => object(json!({"state": {"type": "object"}, "app": {"type": "object"}}), &["state", "app"]),
        "doc_inspect" => object(json!({"project": {"type": "object"}, "activeComp": {"type": ["object", "null"]}}), &["project", "activeComp"]),
        "render_preview" | "render_frame" => object(
            json!({
                "comp": {"type": ["integer", "string", "null"], "description": "Composition id (null for an `item` render)."},
                "item": {"type": "integer", "description": "The footage or solid item rendered (with `item`)."},
                "layer": {"type": "integer", "description": "The soloed layer (with `layer`)."},
                "time": {"type": "number", "description": "Comp time in seconds."},
                "frame": int,
                "width": int, "height": int,
                "scale": {"type": "number", "description": "Output pixels per comp pixel."},
                "region": {"type": "array", "items": {"type": "number"}, "description": "The rendered [x, y, w, h] (with `region`)."},
                "overlays": {"type": "array", "items": {"type": "string"}},
                "layers": {"type": "array", "items": {"type": "object"}, "description": "Drawn layers with comp-space `bounds` and `anchor` (with bounds / anchor / motionPath overlays)."},
                "path": {"type": "string", "description": "Where the PNG was written (with `path`)."}
            }),
            &["comp", "time", "width", "height"],
        ),
        "render_frames" => object(
            json!({
                "times": {"type": "array", "items": {"type": "number"}}, "frames": {"type": "array", "items": int},
                "count": int, "requested": int, "cols": int, "rows": int,
                "cell": {"type": "array", "items": int}, "width": int, "height": int,
                "path": {"type": "string"}, "gif": {"type": "string"}
            }),
            &["times", "frames", "count", "cols", "rows", "cell", "width", "height"],
        ),
        "render_snapshot" => object(
            json!({
                "name": {"type": "string"}, "comp": int, "time": {"type": "number", "description": "Comp time in seconds."},
                "width": int, "height": int,
                "snapshots": {"type": "array", "items": {"type": "string"}, "description": "Kept snapshot names, oldest first."},
                "evicted": {"type": "array", "items": {"type": "string"}, "description": "Snapshots dropped to make room."}
            }),
            &["name", "comp", "time", "width", "height", "snapshots"],
        ),
        "render_compare" => {
            let num = json!({"type": "number"});
            let rect = json!({"type": "array", "items": int, "minItems": 4, "maxItems": 4});
            object(
                json!({
                    "width": int, "height": int,
                    "max_abs": num, "mean_abs": num, "alpha_max_abs": num,
                    "psnr_db": {"type": ["number", "string"], "description": "dB, or \"inf\" when the colour is identical."},
                    "ssim": num, "changed_pixels": int, "changed_pct": num, "threshold": num, "pass": {"type": "boolean"},
                    "diffBounds": {"type": ["array", "null"], "items": int, "minItems": 4, "maxItems": 4, "description": "[x, y, w, h] of the pixels over the threshold, in compared-frame pixels; null when none."},
                    "region": rect, "min_psnr": num,
                    "a": {"type": "object"}, "b": {"type": "object"},
                    "resampled": {"type": "string", "enum": ["a", "b"]}, "sizes": {"type": "object"},
                    "diff_path": {"type": "string"}, "diff_image": {"type": "array", "items": int, "description": "Size of the inline heatmap image."}
                }),
                &[
                    "width",
                    "height",
                    "max_abs",
                    "mean_abs",
                    "alpha_max_abs",
                    "psnr_db",
                    "ssim",
                    "changed_pixels",
                    "changed_pct",
                    "threshold",
                    "pass",
                    "diffBounds",
                    "a",
                    "b",
                ],
            )
        }
        "undo" | "redo" => object(json!({"steps": int, "undoStack": {"type": "array", "items": {"type": "string"}}}), &["steps", "undoStack"]),
        "history" => {
            object(json!({"states": {"type": "array", "items": {"type": "object"}}, "current": {"type": ["integer", "null"]}, "branches": int}), &["states"])
        }
        "command_batch" => object(
            json!({
                "completed": int, "failed": int,
                "results": {"type": "array", "items": object(json!({"ok": {"type": "boolean"}, "result": {}, "error": {"type": "string"}}), &["ok"])}
            }),
            &["completed", "failed", "results"],
        ),
        "batch" => object(json!({"steps": int, "results": {"type": "array"}}), &["steps", "results"]),
        "save_project" => object(json!({"path": {"type": ["string", "null"]}, "dirty": {"type": "boolean"}}), &["path", "dirty"]),
        "get_layout" => object(
            json!({
                "comp": int, "time": {"type": "number"}, "width": int, "height": int,
                "layers": {"type": "array", "items": object(json!({"id": int, "name": {"type": "string"}, "visible": {"type": "boolean"}, "bounds": {"type": ["array", "null"]}}), &["id", "visible"])},
                "overlaps": {"type": "array", "items": {"type": "object"}}
            }),
            &["comp", "time", "width", "height", "layers"],
        ),
        "lint_frame" => {
            let problem = object(
                json!({
                    "rule": {"type": "string", "enum": effectcraft_engine::lint::RULES},
                    "severity": {"type": "string", "enum": ["error", "warning", "info"]},
                    "layer": int, "layerName": {"type": "string"},
                    "time": {"type": "number", "description": "First sample time it occurs at (comp seconds)."},
                    "until": {"type": "number", "description": "Last sample time it occurs at (comp seconds)."},
                    "message": {"type": "string"},
                    "other": {"type": "integer", "description": "The second layer of a pair (text-overlap, the cover of hidden-visible)."},
                    "otherName": {"type": "string"}
                }),
                &["rule", "severity", "layer", "layerName", "time", "until", "message"],
            );
            object(
                json!({
                    "comp": int, "compName": {"type": "string"},
                    "times": {"type": "array", "items": {"type": "number"}}, "truncated": {"type": "boolean"},
                    "clean": {"type": "boolean", "description": "No errors or warnings (info doesn't count)."},
                    "counts": object(json!({"error": int, "warning": int, "info": int}), &["error", "warning", "info"]),
                    "rules": {"type": "array", "items": {"type": "string"}},
                    "problems": {"type": "array", "items": problem},
                    "notes": {"type": "array", "items": {"type": "string"}}
                }),
                &["comp", "compName", "times", "truncated", "clean", "counts", "problems"],
            )
        }
        "get_motion" => {
            let num = json!({"type": "number"});
            let seg = object(
                json!({"kind": {"type": "string", "enum": ["enter", "exit", "move", "hold", "scale", "rotate", "fade-in", "fade-out"]}, "start": num, "end": num, "from": {}, "to": {}, "direction": {"type": "string"}, "edge": {"type": "string"}}),
                &["kind", "start", "end"],
            );
            let span = object(json!({"start": num, "end": num}), &["start", "end"]);
            object(
                json!({
                    "comp": int, "compName": {"type": "string"}, "start": num, "end": num, "step": num, "truncated": {"type": "boolean"},
                    "layers": {"type": "array", "items": object(json!({"layer": int, "name": {"type": "string"}, "inPoint": num, "outPoint": num, "segments": {"type": "array", "items": seg}, "offscreen": {"type": "array", "items": span}, "summary": {"type": "string"}}), &["layer", "name", "segments", "offscreen", "summary"])},
                    "overlaps": {"type": "array", "items": object(json!({"a": int, "b": int, "aName": {"type": "string"}, "bName": {"type": "string"}, "start": num, "end": num, "maxFraction": num}), &["a", "b", "start", "end", "maxFraction"])}
                }),
                &["comp", "start", "end", "step", "truncated", "layers", "overlaps"],
            )
        }
        "analyze_audio" => {
            let db = json!({"type": ["number", "null"]});
            object(
                json!({
                    "comp": int, "start": {"type": "number"}, "end": {"type": "number"}, "hasAudio": {"type": "boolean"},
                    "peakDbfs": db, "truePeakDbtp": db, "rmsDbfs": db, "integratedLufs": db,
                    "clippedSamples": int, "silences": {"type": "array", "items": {"type": "object"}}, "channels": {"type": "array", "items": {"type": "object"}}
                }),
                &["comp", "start", "end", "hasAudio"],
            )
        }
        _ => return None,
    })
}

/// A JSON result as MCP `structuredContent`, which must be an object: arrays become
/// `{"items": [...]}`, other values `{"value": v}`.
pub fn structured(v: Value) -> Value {
    match v {
        Value::Object(_) => v,
        Value::Array(_) => json!({"items": v}),
        other => json!({"value": other}),
    }
}
/// Tools refused while file access is confined: they drive the live app like a person (menus,
/// shortcuts, Open Recent…) or call control-channel methods directly, past the command checks.
const UNCONFINED_ONLY: &[&str] = &["control", "ui_click", "ui_drag", "ui_key", "ui_type", "ui_set", "script_ui"];

/// Run a tool by name.
pub fn run(b: &mut Backend, name: &str, args: &Value) -> Result<Reply> {
    let t = find(name).ok_or_else(|| Error::BadArgs(format!("unknown tool `{name}`")))?;
    if t.bridge_only && !b.is_bridge() {
        return Err(Error::BadArgs(NEED_BRIDGE.into()));
    }
    if b.confinement().is_some() && UNCONFINED_ONLY.contains(&name) {
        return Err(Error::BadArgs(format!("`{name}` is not available while file access is confined (--automation-read-root/--automation-write-root)")));
    }
    let empty = json!({});
    (t.run)(b, if args.is_null() { &empty } else { args })
}

// ------------------------------------------------------------------ schema fragments

fn schema(props: Value, required: &[&str]) -> Value {
    json!({"type": "object", "properties": props, "required": required, "additionalProperties": false})
}
fn offset_s() -> Value {
    json!({"type": "integer", "minimum": 0, "description": "Paging: index of the first item to return (default 0). Use the reply's `next_offset` to continue."})
}
fn limit_s(what: &str, default: usize) -> Value {
    json!({"type": "integer", "minimum": 1, "description": format!("Paging: at most this many {what} (default {default}). A cut list says so in `truncated` and `next_offset`.")})
}
fn full_s(what: &str) -> Value {
    json!({"type": "boolean", "description": format!("true: {what} with every field (the long form) instead of the compact default.")})
}
fn comp_s() -> Value {
    json!({"type": ["integer", "string"], "description": "Composition item id or name; omit for the active comp."})
}
fn layer_s() -> Value {
    json!({"type": ["integer", "string"], "description": "Layer id (from get_comp), \"#n\" (1-based index from the top) or layer name."})
}
fn path_s() -> Value {
    json!({"type": "string", "description": "Property path from get_layer: match ids joined by `/`, `#n` for the nth child, or `@uid`. E.g. `transform/position`, `transform/opacity`, `transform/scale`, `transform/rotation`, `effects/#1/blurriness`, `masks/#1/feather`, `@57`."})
}
fn item_s() -> Value {
    json!({"type": ["integer", "string"], "description": "Render a project item on its own instead of a comp: a footage or solid item (id or name), at source time `time`; a comp item is the same as `comp`."})
}
fn solo_s() -> Value {
    json!({"type": ["integer", "string"], "description": "Render only this layer (solo, in its place in the comp): id, \"#n\" or name."})
}
fn region_s() -> Value {
    json!({"type": "array", "items": {"type": "number"}, "minItems": 4, "maxItems": 4, "description": "[x, y, w, h] in comp pixels: render just this rectangle at full resolution (clipped to the frame; `max_side` then applies to it). For detail checks on big comps."})
}
fn overlays_s() -> Value {
    json!({"type": ["boolean", "array", "string"], "items": {"type": "string", "enum": ["bounds", "anchor", "motionPath", "safeMargins", "grid", "all"]}, "description": "Draw the viewer's overlays on top: `bounds` (layer boxes tagged `#n name`, in label colours), `anchor`, `motionPath` (anchor path with keyframe squares), `safeMargins`, `grid`, or `all`; true = bounds + anchor. The reply's `layers` lists each drawn layer's comp-space `bounds` and `anchor`."})
}
fn value_s() -> Value {
    json!({"description": "Number for 1-D properties (50), array for vectors ([960,540]), \"#rrggbb\" or [r,g,b,a] (0-1) for colours, bool for checkboxes, string for text/menus."})
}

// ------------------------------------------------------------------ helpers

/// Build a params object from the present, non-null entries.
fn obj(entries: &[(&str, Option<&Value>)]) -> Value {
    let mut m = Map::new();
    for (k, v) in entries {
        if let Some(v) = v.filter(|v| !v.is_null()) {
            m.insert(k.to_string(), v.clone());
        }
    }
    Value::Object(m)
}

fn get<'a>(a: &'a Value, k: &str) -> Option<&'a Value> {
    a.get(k).filter(|v| !v.is_null())
}

fn need<'a>(a: &'a Value, k: &str) -> Result<&'a Value> {
    get(a, k).ok_or_else(|| Error::BadArgs(format!("missing `{k}`")))
}

fn json_reply(v: Value) -> Result<Reply> {
    Ok(Reply::Json(v))
}

/// Accept params given as a JSON string too (some clients stringify nested objects).
fn params_of(v: Option<&Value>) -> Result<Value> {
    match v {
        None => Ok(json!({})),
        Some(Value::String(s)) if s.trim().is_empty() => Ok(json!({})),
        Some(Value::String(s)) => serde_json::from_str(s).map_err(|e| Error::BadArgs(format!("`params` is not valid JSON: {e}"))),
        Some(v) => Ok(v.clone()),
    }
}

// ------------------------------------------------------------------ engine tools

fn list_commands(b: &mut Backend, a: &Value) -> Result<Reply> {
    let schemas = get(a, "schemas").and_then(Value::as_bool).unwrap_or(false).then_some(json!(true));
    let r = b.exec(
        "command.list",
        obj(&[("filter", get(a, "filter")), ("enabledOnly", get(a, "enabled_only")), ("schemas", schemas.as_ref()), ("brief", get(a, "brief"))]),
    )?;
    // Drop empty fields to keep the listing compact.
    let list: Vec<Value> = r
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|mut c| {
            if let Some(o) = c.as_object_mut() {
                o.retain(|_, v| !(v.is_null() || v.as_array().is_some_and(Vec::is_empty) || v.as_str() == Some("{}")));
            }
            c
        })
        .collect();
    json_reply(json!(list))
}

fn execute_command(b: &mut Backend, a: &Value) -> Result<Reply> {
    let id = get(a, "command").or(get(a, "id")).and_then(Value::as_str).ok_or_else(|| Error::BadArgs("missing `command` (e.g. `comp.new`)".into()))?;
    let params = params_of(get(a, "params"))?;
    json_reply(b.exec(id, params)?)
}

fn describe_command(b: &mut Backend, a: &Value) -> Result<Reply> {
    let id = get(a, "command").or(get(a, "id")).and_then(Value::as_str).ok_or_else(|| Error::BadArgs("missing `command` (e.g. `layer.newText`)".into()))?;
    json_reply(b.exec("command.describe", json!({"command": id}))?)
}

fn list_effects(b: &mut Backend, a: &Value) -> Result<Reply> {
    json_reply(b.exec("effect.list", obj(&[("filter", get(a, "filter")), ("brief", get(a, "brief"))]))?)
}

fn list_fonts(b: &mut Backend, a: &Value) -> Result<Reply> {
    json_reply(b.exec("text.fonts", obj(&[("query", get(a, "query")), ("variable", get(a, "variable")), ("rescan", get(a, "rescan"))]))?)
}

/// `effect.apply` on one layer, then set parameters on the new instance by param id: one batch,
/// so one undo step, and a failing value leaves nothing applied.
fn add_effect(b: &mut Backend, a: &Value) -> Result<Reply> {
    let (layer, effect, comp) = (need(a, "layer")?, need(a, "effect")?, get(a, "comp"));
    let count = |t: &Value| effects_group(t).and_then(|g| g.get("children")).and_then(Value::as_array).map_or(0, Vec::len);
    let before = b.exec("layer.tree", obj(&[("layer", Some(layer)), ("comp", comp)]))?;
    let index = count(&before) + 1;
    let mut p = obj(&[("effect", Some(effect)), ("comp", comp)]);
    p["layers"] = json!([layer]);
    let mut steps = vec![json!({"command": "effect.apply", "params": p})];
    let prefix = format!("effects/#{index}");
    let mut set = vec![];
    if let Some(vals) = get(a, "values").and_then(Value::as_object) {
        for (k, v) in vals {
            let path = format!("{prefix}/{k}");
            steps.push(
                json!({"command": "prop.set", "params": obj(&[("layer", Some(layer)), ("comp", comp), ("path", Some(&json!(path))), ("value", Some(v))])}),
            );
            set.push(path);
        }
    }
    // The undo step is named like effect.apply's own ("Apply Gaussian Blur").
    let name = effect.as_str().map(|e| effectcraft_engine::effects::lookup(e).map_or(e, |s| s.name));
    let label = name.map(|n| json!(format!("Apply {n}")));
    let keys: Vec<&String> = get(a, "values").and_then(Value::as_object).map(|v| v.keys().collect()).unwrap_or_default();
    let done = b.exec("engine.batch", obj(&[("steps", Some(&json!(steps))), ("label", label.as_ref())])).map_err(|e| add_effect_error(e, &keys))?;
    let warnings: Vec<Value> = done["results"].as_array().map(|r| r.iter().flat_map(warnings_of).collect()).unwrap_or_default();
    let tree = b.exec("layer.tree", obj(&[("layer", Some(layer)), ("comp", comp)]))?;
    let inst = effects_group(&tree).and_then(|g| g.get("children")).and_then(Value::as_array).and_then(|c| c.get(index - 1)).cloned();
    let mut params = vec![];
    if let Some(i) = &inst {
        flatten(i, &mut params);
    }
    let mut out = json!({"layer": tree["id"], "index": index, "path": prefix, "name": inst.as_ref().map(|i| i["name"].clone()), "set": set, "params": params});
    if !warnings.is_empty() {
        out["warnings"] = json!(warnings);
    }
    json_reply(out)
}

/// An add_effect failure in the words of its arguments: the batch's `step 3 (prop.set): invalid
/// parameters for `prop.set`: …` becomes `values.inBlack: …` (step 1 is the effect itself).
fn add_effect_error(e: Error, keys: &[&String]) -> Error {
    let msg = e.to_string();
    let Some((head, rest)) = msg.strip_prefix("step ").and_then(|m| m.split_once("): ")) else { return e };
    let Some(n) = head.split_whitespace().next().and_then(|n| n.parse::<usize>().ok()) else { return e };
    let rest = rest.strip_suffix("; the batch was rolled back (nothing changed)").map_or(rest.to_string(), |r| format!("{r} (nothing was applied)"));
    let inner = ["prop.set", "effect.apply"].iter().find_map(|c| rest.strip_prefix(&format!("invalid parameters for `{c}`: "))).unwrap_or(&rest).to_string();
    match n.checked_sub(2).and_then(|i| keys.get(i)) {
        Some(k) => Error::BadArgs(format!("values.{k}: {inner}")),
        None => Error::BadArgs(inner),
    }
}

/// The `warnings` of a command result (prop.set / prop.addKey report clamped or coerced values).
fn warnings_of(v: &Value) -> Vec<Value> {
    match v {
        Value::Object(o) => o.get("warnings").and_then(Value::as_array).cloned().unwrap_or_default(),
        Value::Array(a) => a.iter().flat_map(warnings_of).collect(),
        _ => vec![],
    }
}

/// The `effects` group of a `layer.tree` reply.
fn effects_group(tree: &Value) -> Option<&Value> {
    tree["properties"]["children"].as_array()?.iter().find(|c| c["path"] == "effects")
}

fn get_state(b: &mut Backend, _: &Value) -> Result<Reply> {
    let state = b.exec("editor.state", json!({}))?;
    let caps = b.exec("app.capabilities", json!({}))?;
    json_reply(json!({"state": state, "app": caps}))
}

fn run_script(b: &mut Backend, a: &Value) -> Result<Reply> {
    let code = get(a, "code").and_then(Value::as_str).ok_or_else(|| Error::BadArgs("missing `code` (JavaScript)".into()))?;
    let mut p = json!({"code": code});
    if let Some(n) = get(a, "name") {
        p["name"] = n.clone();
    }
    // A time limit by default, so a runaway loop can't hold the session forever; 0 = none.
    p["timeout"] = get(a, "timeout").cloned().unwrap_or(json!(SCRIPT_TIMEOUT));
    json_reply(b.exec("script.run", p)?)
}

/// The default `run_script` time limit, in seconds.
pub const SCRIPT_TIMEOUT: f64 = 300.0;

fn get_project(b: &mut Backend, _: &Value) -> Result<Reply> {
    json_reply(b.exec("project.summary", json!({}))?)
}

fn get_comp(b: &mut Backend, a: &Value) -> Result<Reply> {
    let info = b.exec("comp.info", obj(&[("comp", get(a, "comp"))]))?;
    json_reply(crate::compact::comp_reply(info, a)?)
}

/// Flatten a `layer.tree` property tree into `[{path, type, value, keys?, expression?}]`.
fn flatten(node: &Value, out: &mut Vec<Value>) {
    // A group cut off by `depth` stays visible (instead of vanishing from the flat list).
    if node.get("truncated").and_then(Value::as_bool) == Some(true) {
        out.push(json!({"path": node["path"], "type": "group", "name": node["name"], "truncated": true}));
        return;
    }
    match node.get("children").and_then(Value::as_array) {
        Some(ch) => ch.iter().for_each(|c| flatten(c, out)),
        None => {
            let mut o = json!({"path": node["path"], "type": node["type"], "value": node["value"]});
            if let Some(k) = node.get("keys").and_then(Value::as_array) {
                o["keys"] = json!(k.len());
            }
            if let Some(e) = node.get("expression") {
                o["expression"] = e.clone();
            }
            out.push(o);
        }
    }
}

fn get_layer(b: &mut Backend, a: &Value) -> Result<Reply> {
    let p = obj(&[("layer", Some(need(a, "layer")?)), ("comp", get(a, "comp")), ("depth", get(a, "depth")), ("time", get(a, "time"))]);
    let mut r = b.exec("layer.tree", p)?;
    if get(a, "flat").and_then(Value::as_bool) == Some(true) {
        let mut props = vec![];
        flatten(&r["properties"], &mut props);
        crate::compact::flat_reply(&mut r, props, a)?;
    } else {
        crate::compact::tree_hint(&mut r);
    }
    json_reply(r)
}

fn get_property(b: &mut Backend, a: &Value) -> Result<Reply> {
    let p = obj(&[("layer", Some(need(a, "layer")?)), ("path", Some(need(a, "path")?)), ("comp", get(a, "comp")), ("time", get(a, "time"))]);
    json_reply(crate::compact::property_reply(b.exec("prop.get", p)?, a)?)
}

fn set_property(b: &mut Backend, a: &Value) -> Result<Reply> {
    let (layer, path, comp, time) = (need(a, "layer")?, need(a, "path")?, get(a, "comp"), get(a, "time"));
    let (value, expr) = (get(a, "value"), get(a, "expression"));
    if value.is_none() && expr.is_none() {
        return Err(Error::BadArgs("give `value` and/or `expression`".into()));
    }
    let base = [("layer", Some(layer)), ("path", Some(path)), ("comp", comp)];
    let mut warnings = vec![];
    if let Some(v) = value {
        let mut p = obj(&base);
        p["value"] = v.clone();
        let r = match time {
            Some(t) => {
                p["time"] = t.clone();
                if let Some(tb) = get(a, "timeBase") {
                    p["timeBase"] = tb.clone();
                }
                b.exec("prop.addKey", p)?
            }
            None => b.exec("prop.set", p)?,
        };
        warnings = warnings_of(&r);
    }
    if let Some(e) = expr {
        let mut p = obj(&base);
        p["expression"] = e.clone();
        b.exec("prop.setExpression", p)?;
    }
    let mut r = b.exec("prop.get", obj(&[("layer", Some(layer)), ("path", Some(path)), ("comp", comp), ("time", time)]))?;
    if !warnings.is_empty() {
        r["warnings"] = json!(warnings);
    }
    json_reply(r)
}

fn add_keyframe(b: &mut Backend, a: &Value) -> Result<Reply> {
    let (layer, path, comp) = (need(a, "layer")?, need(a, "path")?, get(a, "comp"));
    let keys: Vec<Value> = match get(a, "keys") {
        Some(Value::Array(k)) => k.clone(),
        Some(_) => return Err(Error::BadArgs("`keys` must be an array of {time, value?}".into())),
        None => vec![obj(&[("time", Some(need(a, "time")?)), ("value", get(a, "value"))])],
    };
    if keys.is_empty() {
        return Err(Error::BadArgs("no keys".into()));
    }
    let base = [("layer", Some(layer)), ("path", Some(path)), ("comp", comp)];
    let mut warnings = vec![];
    for (i, k) in keys.iter().enumerate() {
        let mut p = obj(&base);
        p["time"] = get(k, "time").ok_or_else(|| Error::BadArgs(format!("keys[{i}]: missing `time`; each key is {{time, value?}}, got {k}")))?.clone();
        if let Some(v) = get(k, "value") {
            p["value"] = v.clone();
        }
        if let Some(tb) = get(a, "timeBase") {
            p["timeBase"] = tb.clone();
        }
        warnings.extend(warnings_of(&b.exec("prop.addKey", p)?));
    }
    let with_warnings = |mut r: Value| {
        if !warnings.is_empty() {
            r["warnings"] = json!(warnings);
        }
        r
    };
    let info = b.exec("prop.get", obj(&base))?;
    if let Some(interp) = get(a, "interpolation").and_then(Value::as_str) {
        // Key selection works on the active comp.
        if let Some(c) = comp {
            b.exec("comp.open", json!({"comp": c}))?;
        }
        let sel: Vec<Value> = keys.iter().map(|k| json!({"layer": info["layer"], "prop": info["uid"], "time": k["time"]})).collect();
        let mut select = json!({"keys": sel});
        if let Some(tb) = get(a, "timeBase") {
            select["timeBase"] = tb.clone();
        }
        b.exec("keys.select", select)?;
        match interp.to_ascii_lowercase().as_str() {
            "easyease" | "ease" => b.exec("keys.easyEase", json!({}))?,
            "easyeasein" => b.exec("keys.easyEase", json!({"which": "in"}))?,
            "easyeaseout" => b.exec("keys.easyEase", json!({"which": "out"}))?,
            i @ ("linear" | "bezier" | "hold") => b.exec("keys.interpolation", json!({"interpolation": i}))?,
            other => return Err(Error::BadArgs(format!("unknown interpolation `{other}` (linear, bezier, hold, easyEase, easyEaseIn, easyEaseOut)"))),
        };
        return json_reply(with_warnings(b.exec("prop.get", obj(&base))?));
    }
    json_reply(with_warnings(info))
}

/// The render arguments shared by render_frame / render_preview / render_frames.
fn render_params(a: &Value, keys: &[&str]) -> Value {
    let entries: Vec<(&str, Option<&Value>)> = keys.iter().map(|k| (*k, get(a, k))).collect();
    obj(&entries)
}

const VIEW_KEYS: &[&str] = &["comp", "item", "layer", "time", "transparent", "region", "overlays"];

fn render_frame(b: &mut Backend, a: &Value) -> Result<Reply> {
    let mut p = render_params(a, VIEW_KEYS);
    p["max_side"] = get(a, "max_side").cloned().unwrap_or(json!(960));
    let (png, mut info) = b.render_view(p)?;
    if let Some(p) = get(a, "path").and_then(Value::as_str) {
        if let Some(c) = b.confinement() {
            c.check(p, crate::confine::Access::Write)?;
        }
        std::fs::write(p, &png).map_err(|e| Error::Other(format!("cannot write {p}: {e}")))?;
        info["path"] = json!(p);
    }
    if get(a, "inline").and_then(Value::as_bool) == Some(false) {
        return json_reply(info);
    }
    Ok(Reply::Image { png, info })
}

fn render_frames(b: &mut Backend, a: &Value) -> Result<Reply> {
    let mut p = render_params(
        a,
        &["comp", "item", "layer", "times", "count", "start", "end", "cols", "max_side", "labels", "transparent", "region", "overlays", "path", "gif"],
    );
    p["base64"] = json!(true);
    let mut r = b.exec("render.contactSheet", p)?;
    let png = take_png(&mut r, "render.contactSheet")?;
    if get(a, "inline").and_then(Value::as_bool) == Some(false) {
        return json_reply(r);
    }
    Ok(Reply::Image { png, info: r })
}

/// Remove and decode the base64 `png` of a render reply.
pub(crate) fn take_png(r: &mut Value, what: &str) -> Result<Vec<u8>> {
    let data = r.as_object_mut().and_then(|o| o.remove("png")).ok_or_else(|| Error::App(format!("{what} returned no `png`")))?;
    crate::base64::decode(data.as_str().unwrap_or_default()).ok_or_else(|| Error::App(format!("{what}: bad base64")))
}

fn render_snapshot(b: &mut Backend, a: &Value) -> Result<Reply> {
    let p = obj(&[
        ("name", get(a, "name")),
        ("comp", get(a, "comp")),
        ("time", get(a, "time")),
        ("max_side", get(a, "max_side")),
        ("transparent", get(a, "transparent")),
    ]);
    json_reply(b.exec("render.snapshot", p)?)
}

/// Longest side of the diff heatmap returned inline.
const DIFF_INLINE_SIDE: u32 = 768;

fn render_compare(b: &mut Backend, a: &Value) -> Result<Reply> {
    need(a, "a")?;
    need(a, "b")?;
    let inline = get(a, "inline").and_then(Value::as_bool) != Some(false);
    let mut p = obj(&[
        ("a", get(a, "a")),
        ("b", get(a, "b")),
        ("threshold", get(a, "threshold")),
        ("min_psnr", get(a, "min_psnr")),
        ("max_side", get(a, "max_side")),
        ("region", get(a, "region")),
        ("amplify", get(a, "amplify")),
        ("diff_path", get(a, "diff_path")),
    ]);
    if inline {
        p["diff_inline"] = json!(true);
    }
    let mut r = b.exec("render.compare", p)?;
    let diff = r.as_object_mut().and_then(|o| o.remove("diff_png"));
    // The heatmap is worth looking at only when something changed.
    let changed = r["changed_pixels"].as_u64().unwrap_or(0) > 0;
    match diff.as_ref().and_then(Value::as_str).filter(|_| changed) {
        Some(b64) => {
            let raw = crate::base64::decode(b64).ok_or_else(|| Error::App("render.compare: bad base64".into()))?;
            let (png, w, h) = recompress_png(&raw, DIFF_INLINE_SIDE)?;
            r["diff_image"] = json!([w, h]);
            Ok(Reply::Image { png, info: r })
        }
        None => json_reply(r),
    }
}

fn frame_source_s() -> Value {
    json!({
        "description": "A snapshot name (string), {\"snapshot\": name}, {\"path\": \"file.png\"} (PNG/JPEG, e.g. a golden or an After Effects reference render) or {\"comp\"?, \"time\"?, \"transparent\"?} to render now ({} = active comp at the current time).",
        "oneOf": [
            {"type": "string"},
            {"type": "object", "properties": {"snapshot": {"type": "string"}}, "required": ["snapshot"], "additionalProperties": false},
            {"type": "object", "properties": {"path": {"type": "string"}}, "required": ["path"], "additionalProperties": false},
            {"type": "object", "properties": {"comp": comp_s(), "time": {"type": "number"}, "transparent": {"type": "boolean"}}, "additionalProperties": false}
        ]
    })
}

fn open_project(b: &mut Backend, a: &Value) -> Result<Reply> {
    if let Some(p) = get(a, "path") {
        b.exec("file.open", json!({"path": p}))?;
    } else if get(a, "demo").and_then(Value::as_bool) == Some(true) {
        b.exec("file.openDemoProject", json!({}))?;
    } else if get(a, "new").and_then(Value::as_bool) == Some(true) {
        b.exec("file.newProject", json!({}))?;
    } else {
        return Err(Error::BadArgs("give `path` (.ecproj), `demo: true` or `new: true`".into()));
    }
    get_project(b, &json!({}))
}

/// `hosts`: where projects are open; `join` switches this server to one of them (a running app or
/// another host with a control port), keeping the agent's name so it works on its own cursor.
fn hosts(b: &mut Backend, a: &Value) -> Result<Reply> {
    let list = b.exec("hosts.list", json!({}))?;
    let Some(join) = get(a, "join") else { return json_reply(list) };
    let hosts = list["hosts"].as_array().cloned().unwrap_or_default();
    let port = match join {
        Value::Number(n) => n.as_u64().and_then(|p| u16::try_from(p).ok()).ok_or_else(|| Error::BadArgs(format!("`join`: {n} is not a port")))?,
        Value::String(project) => {
            let wanted = std::fs::canonicalize(project).map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|_| project.clone());
            let name = std::path::Path::new(project).file_name().map(|n| n.to_string_lossy().into_owned());
            let found = hosts.iter().find(|h| h["project"].as_str() == Some(wanted.as_str())).or_else(|| {
                hosts.iter().find(|h| {
                    name.as_deref()
                        .is_some_and(|n| h["project"].as_str().and_then(|p| std::path::Path::new(p).file_name()).is_some_and(|f| f.to_string_lossy() == n))
                })
            });
            let h =
                found.ok_or_else(|| Error::BadArgs(format!("no running EffectCraft has {project} open (see `hosts`); open it with open_project instead")))?;
            h["port"].as_u64().and_then(|p| u16::try_from(p).ok()).ok_or_else(|| {
                Error::BadArgs(format!(
                    "{project} is open in {} {}, which agents can't join (no control port)",
                    h["kind"].as_str().unwrap_or("a process"),
                    h["pid"]
                ))
            })?
        }
        other => return Err(Error::BadArgs(format!("`join` is a port or a project path, not {other}"))),
    };
    // Leaving a headless project: never drop unsaved work.
    if let Some(s) = b.session()
        && s.is_dirty()
    {
        return Err(Error::BadArgs("this server's project has unsaved changes: save_project first, then join".into()));
    }
    let mut next = Backend::bridge(&port.to_string())?;
    next.set_client(Some(b.client().map(str::to_string).unwrap_or_else(|| format!("mcp-{}", std::process::id()))));
    let summary = next.exec("project.summary", json!({}))?;
    let client = next.client().map(str::to_string);
    *b = next;
    json_reply(json!({"joined": port, "client": client, "project": summary}))
}

fn save_project(b: &mut Backend, a: &Value) -> Result<Reply> {
    match get(a, "path") {
        Some(p) => b.exec("file.saveAs", json!({"path": p}))?,
        None => b.exec("file.save", json!({}))?,
    };
    let s = b.exec("project.summary", json!({}))?;
    json_reply(json!({"path": s["path"], "dirty": s["dirty"]}))
}

fn history(b: &mut Backend, a: &Value, id: &str) -> Result<Reply> {
    let steps = get(a, "steps").and_then(Value::as_u64).unwrap_or(1).max(1);
    let mut done = 0;
    for _ in 0..steps {
        match b.exec(id, json!({})) {
            Ok(_) => done += 1,
            Err(e) if done == 0 => return Err(e),
            Err(_) => break,
        }
    }
    let s = b.exec("project.summary", json!({}))?;
    json_reply(json!({"steps": done, "undoStack": s["undo"]}))
}

fn undo(b: &mut Backend, a: &Value) -> Result<Reply> {
    history(b, a, "edit.undo")
}

fn redo(b: &mut Backend, a: &Value) -> Result<Reply> {
    history(b, a, "edit.redo")
}

fn script_ui_tool(b: &mut Backend, a: &Value) -> Result<Reply> {
    let action = get(a, "action").and_then(Value::as_str).unwrap_or("list");
    let id = match action {
        "list" => "scriptui.list",
        "get" => "scriptui.get",
        "click" => "scriptui.click",
        "set" => "scriptui.set",
        "close" => "scriptui.close",
        other => return Err(Error::BadArgs(format!("unknown action `{other}` (list | get | click | set | close)"))),
    };
    let p = obj(&[("window", get(a, "window")), ("widget", get(a, "widget")), ("value", get(a, "value")), ("result", get(a, "result"))]);
    json_reply(b.exec(id, p)?)
}

fn history_tool(b: &mut Backend, a: &Value) -> Result<Reply> {
    if let Some(g) = get(a, "goto") {
        let p = match g {
            Value::String(id) if id.parse::<u64>().is_err() => json!({"id": id}),
            Value::String(n) => json!({"index": n.parse::<u64>().unwrap_or(0)}),
            v => json!({"index": v}),
        };
        b.exec("edit.history.goto", p)?;
    }
    json_reply(b.exec("edit.history.list", json!({}))?)
}

fn batch_tool(b: &mut Backend, a: &Value) -> Result<Reply> {
    let steps = need(a, "steps")?;
    let steps = match steps {
        Value::String(_) => params_of(Some(steps))?,
        v => v.clone(),
    };
    json_reply(b.exec("engine.batch", obj(&[("steps", Some(&steps)), ("label", get(a, "label")), ("atomic", get(a, "atomic"))]))?)
}

// ------------------------------------------------------------------ analysis tools

fn get_layout(b: &mut Backend, a: &Value) -> Result<Reply> {
    let layers = match get(a, "layers") {
        Some(Value::Array(v)) => Some(json!(v)),
        Some(v) => Some(json!([v])),
        None => None,
    };
    let p =
        obj(&[("comp", get(a, "comp")), ("time", get(a, "time")), ("layers", layers.as_ref()), ("pixels", get(a, "pixels")), ("overlaps", get(a, "overlaps"))]);
    json_reply(b.exec("layer.bounds", p)?)
}

fn lint_frame(b: &mut Backend, a: &Value) -> Result<Reply> {
    let rules = match get(a, "rules") {
        Some(Value::String(r)) => Some(json!([r])),
        other => other.cloned(),
    };
    let p = obj(&[
        ("comp", get(a, "comp")),
        ("time", get(a, "time")),
        ("start", get(a, "start")),
        ("end", get(a, "end")),
        ("step", get(a, "step")),
        ("rules", rules.as_ref()),
        ("minTextPx", get(a, "min_text_px")),
        ("minContrast", get(a, "min_contrast")),
    ]);
    json_reply(b.exec("render.lint", p)?)
}

fn get_motion(b: &mut Backend, a: &Value) -> Result<Reply> {
    let range = [("comp", get(a, "comp")), ("start", get(a, "start")), ("end", get(a, "end")), ("step", get(a, "step"))];
    if let Some(l) = get(a, "layer") {
        let mut p = obj(&range);
        p["layer"] = l.clone();
        return json_reply(b.exec("layer.motion", p)?);
    }
    let layers = match get(a, "layers") {
        Some(Value::Array(v)) => Some(json!(v)),
        Some(v) => Some(json!([v])),
        None => None,
    };
    let mut p = obj(&range);
    if let Some(l) = layers {
        p["layers"] = l;
    }
    json_reply(b.exec("comp.motion", p)?)
}

fn analyze_audio(b: &mut Backend, a: &Value) -> Result<Reply> {
    let waveform = get(a, "waveform").and_then(Value::as_bool).unwrap_or(false);
    let on = json!(true);
    let p = obj(&[
        ("comp", get(a, "comp")),
        ("start", get(a, "start")),
        ("end", get(a, "end")),
        ("layer", get(a, "layer")),
        ("silenceDb", get(a, "silence_db")),
        ("minSilence", get(a, "min_silence")),
        ("truePeak", get(a, "true_peak")),
        ("waveform", waveform.then_some(&on)),
        ("width", get(a, "width")),
        ("height", get(a, "height")),
        ("path", get(a, "path")),
    ]);
    let mut info = b.exec("audio.analyze", p)?;
    let Some(w) = info.get_mut("waveform").and_then(Value::as_object_mut) else { return json_reply(info) };
    // Draw the waveform here from its columns (headless or bridged alike) and drop them.
    let cols: Vec<[f32; 4]> = w
        .remove("columns")
        .and_then(|c| c.as_array().cloned())
        .unwrap_or_default()
        .iter()
        .map(|c| {
            let f = |i: usize| c.get(i).and_then(Value::as_f64).unwrap_or(0.0) as f32;
            [f(0), f(1), f(2), f(3)]
        })
        .collect();
    let height = w.get("height").and_then(Value::as_u64).unwrap_or(160) as u32;
    if !waveform || cols.is_empty() {
        return json_reply(info);
    }
    let (pw, ph, rgba) = effectcraft_engine::audio_analysis::waveform_rgba(&cols, height);
    let png = encode_png(pw, ph, rgba, 0)?;
    Ok(Reply::Image { png, info })
}

// ------------------------------------------------------------------ UI tools (bridge or offscreen UI)

fn screenshot(b: &mut Backend, a: &Value) -> Result<Reply> {
    let max_side = get(a, "max_side").and_then(Value::as_u64).unwrap_or(1600) as u32;
    let keep = get(a, "path").and_then(Value::as_str).map(str::to_string);
    if let (Some(c), Some(p)) = (b.confinement(), &keep) {
        c.check(p, crate::confine::Access::Write)?;
    }
    let path =
        keep.clone().unwrap_or_else(|| std::env::temp_dir().join(format!("effectcraft-mcp-shot-{}.png", std::process::id())).to_string_lossy().into_owned());
    // The app downscales before encoding (a full window is tens of MB otherwise).
    b.control("ui.screenshot", obj(&[("path", Some(&json!(path))), ("panel", get(a, "panel")), ("id", get(a, "id")), ("max_side", Some(&json!(max_side)))]))?;
    let bytes = std::fs::read(&path).map_err(|e| Error::Other(format!("screenshot: {e}")))?;
    if keep.is_none() {
        let _ = std::fs::remove_file(&path);
    }
    let (png, w, h) = recompress_png(&bytes, max_side)?;
    Ok(Reply::Image { png, info: json!({"width": w, "height": h, "path": keep}) })
}

fn passthrough(b: &mut Backend, method: &str, a: &Value) -> Result<Reply> {
    json_reply(b.control(method, a.clone())?)
}

fn ui_inspect(b: &mut Backend, a: &Value) -> Result<Reply> {
    passthrough(b, "ui.inspect", a)
}
fn ui_elements(b: &mut Backend, a: &Value) -> Result<Reply> {
    passthrough(b, "ui.elements", a)
}
fn ui_click(b: &mut Backend, a: &Value) -> Result<Reply> {
    passthrough(b, "ui.click", a)
}
fn ui_drag(b: &mut Backend, a: &Value) -> Result<Reply> {
    passthrough(b, "ui.drag", a)
}
fn ui_key(b: &mut Backend, a: &Value) -> Result<Reply> {
    passthrough(b, "ui.key", a)
}
fn ui_type(b: &mut Backend, a: &Value) -> Result<Reply> {
    passthrough(b, "ui.type", a)
}
fn ui_set(b: &mut Backend, a: &Value) -> Result<Reply> {
    passthrough(b, "ui.set", a)
}
fn control(b: &mut Backend, a: &Value) -> Result<Reply> {
    let m = need(a, "method")?.as_str().ok_or_else(|| Error::BadArgs("`method` must be a string".into()))?;
    let p = params_of(get(a, "params"))?;
    json_reply(b.control(m, p)?)
}

// ------------------------------------------------------------------ the catalogue

static TOOLS: &[ToolDef] = &[
    ToolDef {
        name: "list_commands",
        description: "List engine commands: id, label, menu path, shortcut, `params` doc (e.g. `{name?, width?, height?, frameRate?, duration? (s)}`) and whether it can run now (`why` if not). Every menu item, shortcut and panel gesture maps to one of these ids; run them with command_run. The full list is large (~130 KB): narrow with `filter` (e.g. `layer.new`, `keys`, `effect`) or pass `brief: true` for just ids and labels, then describe_command for one command's parameters.",
        bridge_only: false,
        schema: || {
            schema(
                json!({
                    "filter": {"type": "string", "description": "Only commands whose id or label contains this text (case-insensitive)."},
                    "enabled_only": {"type": "boolean", "description": "Only commands that can run right now."},
                    "schemas": {"type": "boolean", "description": "Also return each command's parameter JSON Schema (`schema`)."},
                    "brief": {"type": "boolean", "description": "Only {id, label} per command (~40 KB instead of ~125 KB); describe_command gives one command's params."}
                }),
                &[],
            )
        },
        run: list_commands,
    },
    ToolDef {
        name: "describe_command",
        description: "One command in full: id, label, menu path, shortcut, the `params` doc, a JSON Schema of its parameters (`schema`: properties, required, additionalProperties) and whether it can run now (`why` if not). Use before command_run when unsure of the parameters; command_run rejects unknown keys.",
        bridge_only: false,
        schema: || {
            schema(
                json!({"command": {"type": "string", "description": "Command id, e.g. `layer.newText`."}, "id": {"type": "string", "description": "Alias of `command`."}}),
                &[],
            )
        },
        run: describe_command,
    },
    ToolDef {
        name: "execute_command",
        description: "Run any engine command by id with JSON params; edits are undoable. Examples: comp.new {\"name\":\"Main\",\"width\":1920,\"height\":1080,\"frameRate\":30,\"duration\":5}; layer.newText {\"text\":\"Hello\",\"size\":120,\"fill\":\"#ffffff\"} (returns {\"layer\":id}); layer.newSolid {\"color\":\"#3366ff\"}; layer.newShape; effect.apply {\"layer\":3,\"effect\":\"Gaussian Blur\"}; time.set {\"time\":2.5}; layer.select {\"layers\":[3]}. Layers are referenced by id, \"#n\" or name; times are seconds.",
        bridge_only: false,
        schema: || {
            schema(
                json!({
                    "command": {"type": "string", "description": "Command id, e.g. `comp.new`, `layer.newText`, `effect.apply` (see command_list)."},
                    "id": {"type": "string", "description": "Alias of `command`."},
                    "params": {"type": "object", "description": "Command parameters as documented by command_list.", "additionalProperties": true}
                }),
                &[],
            )
        },
        run: execute_command,
    },
    ToolDef {
        name: "run_script",
        description: "Run JavaScript with an After Effects-style scripting object model (app, app.project, items.addComp, comp.layers.addSolid/addText/addShape/addNull/addCamera/addLight, layer.property(\"ADBE Transform Group\").property(\"ADBE Position\").setValueAtTime(t, v), Effects.addProperty(\"ADBE Gaussian Blur 2\"), expressions, markers, renderQueue…). Every edit is an undoable engine command; app.beginUndoGroup/endUndoGroup make one undo step. Returns {ok, result (last expression value), output (writeLn/$.writeln/alert text), error: {message, line, column} | null}. Stops after `timeout` seconds (default 300, 0 = no limit) or on MCP notifications/cancelled (an open undo group is rolled back; other edits are kept). File access is limited to the project folder unless the user allows scripts to write files.",
        bridge_only: false,
        schema: || {
            schema(
                json!({
                    "code": {"type": "string", "description": "JavaScript source, e.g. `var c = app.project.items.addComp(\"Main\", 1920, 1080, 1, 5, 30); c.layers.addText(\"Hi\"); c.numLayers`."},
                    "name": {"type": "string", "description": "Script name used in error messages (default `script`)."},
                    "timeout": {"type": "number", "minimum": 0, "description": "Stop the script after this many seconds (default 300; 0 = no limit)."}
                }),
                &["code"],
            )
        },
        run: run_script,
    },
    ToolDef {
        name: "get_project",
        description: "Project overview: file path, dirty flag, bit depth, every item (id, name, type Composition/Footage/Solid/Folder, size, duration, layer count), the active comp id, current time (s), selected layers and the undo stack.",
        bridge_only: false,
        schema: || schema(json!({}), &[]),
        run: get_project,
    },
    ToolDef {
        name: "get_comp",
        description: "A composition: id, name, size, frame rate, duration, work area, background, `layerCount` and its layers top to bottom as compact rows {index, id, name, type, in, out} plus only what isn't the default: start (s, when ≠ 0), stretch (≠ 100), parent, trackMatte, blendMode (≠ Normal), switches (tags of the differing ones: hidden, 3d, solo, locked, shy, adjustment, motion_blur, guide, collapse, fx_off, audio_off, quality:…, sampling:…, frame_blend:…). Paged: up to `limit` layers (default 200) from `offset`; `next_offset` is null when every layer was returned, otherwise `truncated` says how to get the rest. `full: true` returns every field of every layer (label, all switches; long). Layer ids feed get_layer, get_property/set_property and commands.",
        bridge_only: false,
        schema: || {
            schema(json!({"comp": comp_s(), "full": full_s("layers"), "offset": offset_s(), "limit": limit_s("layers", crate::compact::COMP_LIMIT)}), &[])
        },
        run: get_comp,
    },
    ToolDef {
        name: "get_layer",
        description: "A layer's property tree (transform, masks, effects, text, shape contents, ...): leaf properties have a `path` usable with get_property/set_property/add_keyframe, plus uid, match id, name, type, value at `time`, keyframes and expression. Groups contain children; inspect them here, then select a leaf path. `flat: true` returns just a list of {path, type, value, keys (count), expression} (compact), paged: up to `limit` (default 150) from `offset`, with `total`, `returned` and `next_offset` (null when done; `truncated` when cut). A big tree carries a `hint` to use flat paging, `depth` or get_property instead.",
        bridge_only: false,
        schema: || {
            schema(
                json!({
                    "layer": layer_s(), "comp": comp_s(),
                    "time": {"type": "number", "description": "Comp time in seconds for values (default: current time)."},
                    "depth": {"type": "integer", "description": "Group depth to expand (default all)."},
                    "flat": {"type": "boolean", "description": "Return a flat list of properties instead of the tree."},
                    "offset": offset_s(), "limit": limit_s("properties (flat: true)", crate::compact::FLAT_LIMIT)
                }),
                &["layer"],
            )
        },
        run: get_layer,
    },
    ToolDef {
        name: "get_property",
        description: "Read one property: type, value at `time` (comp seconds; default the current time), whether it is animated, its expression and its keyframes, compact: {time (layer s), value (rounded to 4 decimals), compTime (only when it differs), in/out (only when not Linear)}. Keyframes are paged: `keyCount` total, up to `limit` (default 100) from `offset`, `next_offset` null when done (`truncated` when cut). `full: true` gives every key field (layerTime, compTime, in/out always).",
        bridge_only: false,
        schema: || {
            schema(
                json!({"layer": layer_s(), "path": path_s(), "comp": comp_s(), "time": {"type": "number", "description": "Comp time in seconds."}, "full": full_s("keyframes"), "offset": offset_s(), "limit": limit_s("keyframes", crate::compact::KEY_LIMIT)}),
                &["layer", "path"],
            )
        },
        run: get_property,
    },
    ToolDef {
        name: "set_property",
        description: "Set a property. Without `time`: sets the static value (or a key at the current time if already animated). With `time`: adds/replaces a keyframe there, turning the stopwatch on. `expression` sets an After Effects-style JS expression (\"\" removes it). Returns the property as get_property does, plus `warnings` when the value was clamped to its valid range or reshaped (a number filling a vector, missing components set to 0). Popup properties take an option index (0-based) or name. Examples: {\"layer\":3,\"path\":\"transform/opacity\",\"value\":50}; {\"layer\":\"Title\",\"path\":\"transform/position\",\"value\":[200,540],\"time\":0}; {\"layer\":3,\"path\":\"transform/rotation\",\"expression\":\"time*90\"}.",
        bridge_only: false,
        schema: || {
            schema(
                json!({
                    "layer": layer_s(), "path": path_s(), "value": value_s(), "comp": comp_s(),
                    "time": {"type": "number", "description": "Keyframe time in seconds (layer time; equals comp time unless the layer is offset/stretched)."},
                    "timeBase": {"type": "string", "enum": ["layer", "comp"], "description": "What `time` counts: layer time (the default) or comp time, as the Timeline shows it."},
                    "expression": {"type": "string", "description": "Expression text; empty string removes the expression."}
                }),
                &["layer", "path"],
            )
        },
        run: set_property,
    },
    ToolDef {
        name: "add_keyframe",
        description: "Add keyframes to a property (turns the stopwatch on): one with `time` (+ optional `value`, default the current value) or many with `keys: [{time, value}]`, then optionally set their `interpolation`: linear, bezier, hold, easyEase, easyEaseIn, easyEaseOut. Example: {\"layer\":1,\"path\":\"transform/position\",\"keys\":[{\"time\":0,\"value\":[0,540]},{\"time\":2,\"value\":[1920,540]}],\"interpolation\":\"easyEase\"}.",
        bridge_only: false,
        schema: || {
            schema(
                json!({
                    "layer": layer_s(), "path": path_s(), "comp": comp_s(),
                    "time": {"type": "number", "description": "Key time in seconds (layer time)."},
                    "timeBase": {"type": "string", "enum": ["layer", "comp"], "description": "What `time` counts: layer time (the default) or comp time, as the Timeline shows it."},
                    "value": value_s(),
                    "keys": {"type": "array", "items": {"type": "object", "properties": {"time": {"type": "number"}, "value": value_s()}, "required": ["time"]}, "description": "Several keys at once."},
                    "interpolation": {"type": "string", "enum": ["linear", "bezier", "hold", "easyEase", "easyEaseIn", "easyEaseOut"]}
                }),
                &["layer", "path"],
            )
        },
        run: add_keyframe,
    },
    ToolDef {
        name: "add_effect",
        description: "Apply an effect to a layer and optionally set its parameters in one undoable call. `effect` is an id (`ec.blur.gaussian`) or After Effects name (`Gaussian Blur`; see list_effects); `values` maps parameter ids to values, e.g. {\"blurriness\": 12}. One undo step; if a value fails, nothing is applied. Returns the instance `path` (`effects/#n`, for set_property / add_keyframe) and its parameters with their paths and current values; values that were clamped to a parameter's range or otherwise adjusted are listed in `warnings`.",
        bridge_only: false,
        schema: || {
            schema(
                json!({
                    "layer": layer_s(), "comp": comp_s(),
                    "effect": {"type": "string", "description": "Effect id or name, e.g. `Gaussian Blur`, `Glow`, `Curves`, `ec.color.levels`."},
                    "values": {"type": "object", "description": "Parameter id -> value (ids are in the returned `params` paths), e.g. {\"blurriness\": 12}.", "additionalProperties": true}
                }),
                &["layer", "effect"],
            )
        },
        run: add_effect,
    },
    ToolDef {
        name: "list_effects",
        description: "The effect registry: id, After Effects name, category, GPU and 32-bpc support, and each parameter (id, name, default, valid `range` of sliders, popup `options`). The full list is large (~360 KB): narrow with `filter` (id, name or category text) or pass `brief: true` for just ids and names (~19 KB). Apply with add_effect.",
        bridge_only: false,
        schema: || {
            schema(
                json!({
                    "filter": {"type": "string", "description": "Only effects whose id, name or category contains this text."},
                    "brief": {"type": "boolean", "description": "Only {id, name} per effect (no parameters)."}
                }),
                &[],
            )
        },
        run: list_effects,
    },
    ToolDef {
        name: "list_fonts",
        description: "The font families text layers can use, bundled and installed on this machine: family, styles, origin (bundled / system / user), whether it is a variable font and the name in the font's own language. Narrow with `query` and `variable`; `rescan` picks up fonts installed since launch. Set one with command_run `layer.setText {\"layer\", \"font\", \"style\"}`.",
        bridge_only: false,
        schema: || {
            schema(
                json!({
                    "query": {"type": "string", "description": "Only families whose name (English or own-language) contains this text."},
                    "variable": {"type": "boolean", "description": "Only variable fonts (families with variation axes)."},
                    "rescan": {"type": "boolean", "description": "Look for fonts installed since launch first."}
                }),
                &[],
            )
        },
        run: list_fonts,
    },
    ToolDef {
        name: "get_state",
        description: "Where things stand: the editor state (active comp, current time, selected layers/keys/items, open comps, tool, work area) plus what this build can do (`app`: version, command and effect counts, GPU effects, import/export formats, After Effects parity summary). Cheap; call it to re-orient after a series of edits.",
        bridge_only: false,
        schema: || schema(json!({}), &[]),
        run: get_state,
    },
    ToolDef {
        name: "render_frame",
        description: "Render a composition frame (default: the active comp at the current time) and return it as a PNG image, longest side `max_side` (default 960, 0 = full size). `region` renders one rectangle at full resolution (detail checks); `layer` renders one layer solo; `item` renders a footage or solid item on its own; `overlays` draws layer bounds, anchor points, motion paths, safe margins or the grid on top and lists each layer's comp-space bounds. `path` also writes the PNG to disk; `inline: false` returns only the metadata. Use it to check your work; render_frames shows several times at once.",
        bridge_only: false,
        schema: || {
            schema(
                json!({
                    "comp": comp_s(),
                    "item": item_s(),
                    "layer": solo_s(),
                    "time": {"type": "number", "description": "Comp time in seconds (default: current time); source time for `item`."},
                    "max_side": {"type": "integer", "description": "Longest side in pixels (default 960; 0 = full size of the frame or region)."},
                    "region": region_s(),
                    "overlays": overlays_s(),
                    "path": {"type": "string", "description": "Also write the PNG here."},
                    "inline": {"type": "boolean", "description": "Include the image in the reply (default true)."},
                    "transparent": {"type": "boolean", "description": "Keep the frame's alpha (RGBA PNG, as an RGB + Alpha render writes it) instead of compositing over the comp's background colour."}
                }),
                &[],
            )
        },
        run: render_frame,
    },
    ToolDef {
        name: "render_frames",
        description: "Several frames on ONE contact-sheet PNG, each cell labelled with its time and frame number: check an animation in one call. `times` (seconds) or `count` frames (default 6, at most 64) spread evenly over `start`..`end` (default the work area). `max_side` is the sheet's longest side (default 1600); `cols` overrides the grid. Takes render_preview's `layer`, `item`, `region`, `overlays` and `transparent`. `path` also writes the sheet, `gif` also writes an animated GIF of the frames. Returns the image plus {times, frames, cols, rows, cell, width, height}.",
        bridge_only: false,
        schema: || {
            schema(
                json!({
                    "comp": comp_s(),
                    "item": item_s(),
                    "layer": solo_s(),
                    "times": {"type": "array", "items": {"type": "number"}, "description": "Comp times in seconds (at most 64)."},
                    "count": {"type": "integer", "minimum": 1, "description": "Frames spread evenly from `start` to `end` when `times` is not given (default 6, capped at 64)."},
                    "start": {"type": "number", "description": "First time in seconds (default: work area start)."},
                    "end": {"type": "number", "description": "End time in seconds, exclusive (default: work area end)."},
                    "cols": {"type": "integer", "minimum": 1, "description": "Columns (default: a grid close to 16:10)."},
                    "max_side": {"type": "integer", "description": "Longest side of the whole sheet in pixels (default 1600, 64-8192; 0 = 8192). Cells never exceed the frame's own size."},
                    "labels": {"type": "boolean", "description": "Label each cell with its time and frame number (default true)."},
                    "region": region_s(),
                    "overlays": overlays_s(),
                    "transparent": {"type": "boolean", "description": "Keep alpha (transparent gutters) instead of compositing over the comp background."},
                    "path": {"type": "string", "description": "Also write the sheet PNG here."},
                    "gif": {"type": "string", "description": "Also write an animated GIF of the frames here (delays follow the times)."},
                    "inline": {"type": "boolean", "description": "Include the image in the reply (default true)."}
                }),
                &[],
            )
        },
        run: render_frames,
    },
    ToolDef {
        name: "get_layout",
        description: "Where layers are on screen at a comp time (layer.bounds), without rendering: per layer whether it is drawn (`visible`; `why` not: in/out, Video switch, opacity 0, solo, guide...), its comp-space axis-aligned `bounds` [x,y,w,h] after transform, parenting and (3D) the active camera, the four `corners` (rotated layers), `center`, `anchorInComp`, the raw `position`/`anchor`, `onScreen` (0-1 share inside the comp frame) and the layer-space `source` rect (sourceRectAtTime). `pixels: true` also measures the drawn pixels (`pixelBounds`: masks, effects, styles); `overlaps: true` lists pairwise overlaps of the visible layers. Use it to check a title is centred, on screen or clear of other layers.",
        bridge_only: false,
        schema: || {
            schema(
                json!({
                    "comp": comp_s(),
                    "time": {"type": "number", "description": "Comp time in seconds (default: current time)."},
                    "layers": {"type": "array", "items": layer_s(), "description": "Layers to measure (default every layer)."},
                    "pixels": {"type": "boolean", "description": "Also measure the drawn pixels incl. effects (renders each layer)."},
                    "overlaps": {"type": "boolean", "description": "Also return pairwise overlaps {layers, area, fraction} of the visible layers."}
                }),
                &[],
            )
        },
        run: get_layout,
    },
    ToolDef {
        name: "lint_frame",
        description: "Design lint (render.lint): plain-language design problems in a comp at one time (`time`, default the current time) or over `start`..`end` (sampled every `step` s plus in points and keyframes, at most 120 samples). Rules: text-too-small (on-screen font size under min_text_px, default comp height / 30), low-contrast (WCAG ratio of text against what is under it under min_contrast, default 3:1), outside-title-safe (text), outside-action-safe, cut-off (partly outside the frame), text-overlap, hidden-visible (switched on but off-screen, transparent or covered), off-beat (keyframes and in points just missing the audio's beat). Returns {clean, counts, problems: [{rule, severity, layer, layerName, time, until, message, other?}]}; each message says what to fix. Run it before calling a design done.",
        bridge_only: false,
        schema: || {
            schema(
                json!({
                    "comp": comp_s(),
                    "time": {"type": "number", "description": "One comp time in seconds (default: current time). Use start/end for a range instead."},
                    "start": {"type": "number", "description": "Range start, comp seconds (default 0 when end is given)."},
                    "end": {"type": "number", "description": "Range end, comp seconds (default the comp's duration when start is given)."},
                    "step": {"type": "number", "description": "Seconds between range samples (default 0.5)."},
                    "rules": {"type": "array", "items": {"type": "string", "enum": effectcraft_engine::lint::RULES}, "description": "Only these rules (default all)."},
                    "min_text_px": {"type": "number", "description": "Smallest legible font size on screen, comp pixels (default comp height / 30)."},
                    "min_contrast": {"type": "number", "description": "Smallest acceptable WCAG contrast ratio (default 3)."}
                }),
                &[],
            )
        },
        run: lint_frame,
    },
    ToolDef {
        name: "get_motion",
        description: "How layers move over a time range, in words and numbers, without rendering (comp.motion / layer.motion): samples each layer's comp-space content box, opacity, scale and rotation (default the whole comp, every frame or coarser so a layer gets at most 600 samples; keyframe times are always sampled) and returns per layer `segments` {kind: enter|exit|move|hold|scale|rotate|fade-in|fade-out, start, end, from, to, direction (move/enter/exit: left, up-right...), edge (enter/exit: the frame edge)}, `offscreen` intervals and a one-sentence `summary` (\"Logo flies in from the left 0–1s, holds 1–4s, fades out 4–5s.\"), plus `overlaps` {a, b, start, end, maxFraction} between drawn layers (full-frame backgrounds left out). Times are comp seconds. Use it to check an animation's timing and choreography; render_frames shows the frames.",
        bridge_only: false,
        schema: || {
            schema(
                json!({
                    "comp": comp_s(),
                    "layer": layer_s(),
                    "layers": {"type": "array", "items": layer_s(), "description": "Layers to describe (default every layer of the comp); `layer` describes one."},
                    "start": {"type": "number", "description": "Start, comp seconds (default 0)."},
                    "end": {"type": "number", "description": "End, comp seconds (default the comp's duration)."},
                    "step": {"type": "number", "description": "Sampling step in seconds (default one frame, coarser for comps over ~10 s so a layer gets at most 600 samples)."}
                }),
                &[],
            )
        },
        run: get_motion,
    },
    ToolDef {
        name: "analyze_audio",
        description: "Measure the comp's audio mix (what preview and export play) over [start, end) seconds (audio.analyze): peakDbfs, truePeakDbtp (4x oversampled), rmsDbfs, integratedLufs (ITU-R BS.1770, gated), momentary / short-term max LUFS, clippedSamples, silent ranges (below silence_db for at least min_silence s) and per-channel stats; null levels mean silence. `layer` measures one layer alone. `waveform: true` returns a waveform image (left channel on top, red where it clips). A comp without audio returns hasAudio: false.",
        bridge_only: false,
        schema: || {
            schema(
                json!({
                    "comp": comp_s(),
                    "start": {"type": "number", "description": "Start, comp seconds (default 0)."},
                    "end": {"type": "number", "description": "End, comp seconds (default the comp's duration)."},
                    "layer": layer_s(),
                    "silence_db": {"type": "number", "description": "Silence threshold in dBFS (default -60)."},
                    "min_silence": {"type": "number", "description": "Shortest silence reported, seconds (default 0.25)."},
                    "true_peak": {"type": "boolean", "description": "Estimate the true peak (default true)."},
                    "waveform": {"type": "boolean", "description": "Return a waveform image."},
                    "width": {"type": "integer", "description": "Waveform width in pixels (default 800)."},
                    "height": {"type": "integer", "description": "Waveform height in pixels (default 160)."},
                    "path": {"type": "string", "description": "Also write the waveform PNG here."}
                }),
                &[],
            )
        },
        run: analyze_audio,
    },
    ToolDef {
        name: "render_snapshot",
        description: "Render a frame (default: the active comp at the current time) and keep it in the session under `name`, for render_compare. Verify an edit by looking: render_snapshot {\"name\":\"before\"} -> edit -> render_compare {\"a\":\"before\",\"b\":{}}. Up to 16 are kept (oldest dropped); taking a name again replaces it.",
        bridge_only: false,
        schema: || {
            schema(
                json!({
                    "name": {"type": "string", "description": "Snapshot name."},
                    "comp": comp_s(),
                    "time": {"type": "number", "description": "Comp time in seconds (default: current time)."},
                    "max_side": {"type": "integer", "minimum": 0, "description": "Longest side in pixels (default 0 = full comp size)."},
                    "transparent": {"type": "boolean", "description": "Keep the frame's alpha instead of compositing over the comp background."}
                }),
                &["name"],
            )
        },
        run: render_snapshot,
    },
    ToolDef {
        name: "render_compare",
        description: "Compare two frames: snapshots (render_snapshot), image files (goldens, After Effects reference renders) or frames rendered now. Returns {width, height, max_abs, mean_abs (8-bit levels), alpha_max_abs, psnr_db (\"inf\" when identical), ssim, changed_pixels, changed_pct, pass, diffBounds ([x, y, w, h] box of the changed pixels, null when none)} and, when something changed, the diff heatmap as an image (red -> yellow -> white by size of difference, over frame a dimmed to grey). `pass` = no channel differs by more than `threshold` levels (default 1), or with `min_psnr` PSNR >= min_psnr. Sizes: `max_side` scales both down first; the same aspect ratio at different sizes compares at the smaller size; different aspect ratios are an error. `region` compares just one rectangle.",
        bridge_only: false,
        schema: || {
            schema(
                json!({
                    "a": frame_source_s(),
                    "b": frame_source_s(),
                    "threshold": {"type": "number", "minimum": 0, "maximum": 255, "description": "Per-channel tolerance in 8-bit levels for `changed_pixels` and `pass` (default 1)."},
                    "min_psnr": {"type": "number", "minimum": 0, "description": "Pass when PSNR (dB) is at least this, instead of the exact threshold test (e.g. 35 for another app's render)."},
                    "max_side": {"type": "integer", "minimum": 0, "description": "Scale both down to this longest side before comparing (0 = full size, the default)."},
                    "region": {"type": "array", "items": {"type": "number"}, "minItems": 4, "maxItems": 4, "description": "[x, y, w, h] in pixels of the compared frames (comp pixels unless scaled): metrics, diffBounds and the heatmap cover only this rectangle (clipped to the frame)."},
                    "amplify": {"type": "number", "minimum": 1, "maximum": 255, "description": "Heatmap gain (default 8: a 32-level difference is white)."},
                    "diff_path": {"type": "string", "description": "Also write the heatmap PNG here (full size)."},
                    "inline": {"type": "boolean", "description": "Include the heatmap image in the reply when something changed (default true)."}
                }),
                &["a", "b"],
            )
        },
        run: render_compare,
    },
    ToolDef {
        name: "hosts",
        description: "Where projects are open: every running EffectCraft (the desktop app, headless servers) with its control port and project. `join` (a port, or the path of a project one of them has open) switches this server onto that host, as your own `client`, so you work in the same project as the user and other agents instead of on a copy (claim your comp with command_run comp.claim). Refused while this server's own project has unsaved changes.",
        bridge_only: false,
        schema: || {
            schema(json!({"join": {"type": ["integer", "string"], "description": "A host's control port, or the path of a project a host has open."}}), &[])
        },
        run: hosts,
    },
    ToolDef {
        name: "open_project",
        description: "Open a project: `path` to an .ecproj file, `demo: true` for the built-in demo project, or `new: true` for an empty project. Returns the project overview.",
        bridge_only: false,
        schema: || {
            schema(json!({"path": {"type": "string", "description": "Path of an .ecproj file."}, "demo": {"type": "boolean"}, "new": {"type": "boolean"}}), &[])
        },
        run: open_project,
    },
    ToolDef {
        name: "save_project",
        description: "Save the project to its current file, or to `path` (Save As, .ecproj).",
        bridge_only: false,
        schema: || schema(json!({"path": {"type": "string", "description": "Destination .ecproj path (Save As)."}}), &[]),
        run: save_project,
    },
    ToolDef {
        name: "undo",
        description: "Undo the last `steps` edits (default 1). Returns the remaining undo stack.",
        bridge_only: false,
        schema: || schema(json!({"steps": {"type": "integer", "minimum": 1}}), &[]),
        run: undo,
    },
    ToolDef {
        name: "redo",
        description: "Redo `steps` undone edits (default 1).",
        bridge_only: false,
        schema: || schema(json!({"steps": {"type": "integer", "minimum": 1}}), &[]),
        run: redo,
    },
    ToolDef {
        name: "history",
        description: "The branching undo history (History panel): without arguments lists every state {index, id, label, parent, depth (0 = working line, >0 = branch), current, future}; undoing and then editing keeps the undone states as a branch. With `goto` (an index or id from the listing) jumps to that state, on any branch, without losing the others.",
        bridge_only: false,
        schema: || schema(json!({"goto": {"type": ["integer", "string"], "description": "State index or id to jump to (edit.history.goto)."}}), &[]),
        run: history_tool,
    },
    ToolDef {
        name: "script_ui",
        description: "Drive ScriptUI windows that scripts opened (dialogs, palettes, dockable panels). action `list` (default): open windows; `get`: a window's control tree (type, name, text, value, checked, items, selection, bounds); `click`: press a button / toggle a checkbox / pick a radio button; `set`: type text, move a slider, pick a list item (index or text); `close`: close a window (`result` for a dialog's show(), default 2 = Cancel). Controls by id, \"#id\", properties.name or text. A dialog's script resumes when the dialog closes.",
        bridge_only: false,
        schema: || {
            schema(
                json!({
                    "action": {"type": "string", "enum": ["list", "get", "click", "set", "close"]},
                    "window": {"type": ["integer", "string"], "description": "Window id or title (optional when one is open)."},
                    "widget": {"type": ["integer", "string"], "description": "Control id, \"#id\", properties.name or text."},
                    "value": {"description": "For `set`: text, number, bool, item index or item text."},
                    "result": {"type": "integer", "description": "For `close`: what the dialog's show() returns."}
                }),
                &[],
            )
        },
        run: script_ui_tool,
    },
    ToolDef {
        name: "screenshot",
        description: "Screenshot of the app window (the live one when bridged, the offscreen one with `mcp --ui`), or one panel (`Composition`, `Timeline`, `EffectControls`, `Project`, ...) or element id, as PNG (longest side `max_side`, default 1600). Use it to check what the UI looks like after an action.",
        bridge_only: true,
        schema: || {
            schema(
                json!({
                    "panel": {"type": "string", "description": "Panel to crop to."},
                    "id": {"type": "string", "description": "Element id (from ui_elements) to crop to."},
                    "max_side": {"type": "integer"},
                    "path": {"type": "string", "description": "Keep the full-size PNG at this path."}
                }),
                &[],
            )
        },
        run: screenshot,
    },
    ToolDef {
        name: "ui_inspect",
        description: "UI state: window size, tool, workspace, panels, viewer and timeline view state, playback, active comp, selection, open dialog, render time.",
        bridge_only: true,
        schema: || schema(json!({}), &[]),
        run: ui_inspect,
    },
    ToolDef {
        name: "ui_elements",
        description: "Interactive elements drawn in the last frame: id, label, rect [x,y,w,h] in points. Stable ids such as `tools.Selection`, `panel.Timeline`, `panel.tab.EffectControls`, `project.item.<id>`, `viewer.comp`, `timeline.layer.<id>.bar`, `timeline.prop.<uid>.stopwatch`, `timeline.cti`, `effects.item.<name>`. Filter with `prefix`.",
        bridge_only: true,
        schema: || schema(json!({"prefix": {"type": "string", "description": "Only ids starting with this, e.g. `tools.`, `timeline.layer.`."}}), &[]),
        run: ui_elements,
    },
    ToolDef {
        name: "ui_click",
        description: "Click an element by `id` (from ui_elements) or at `x`,`y` (points). `button`: left|right|middle, `count`: 2 for double-click, `modifiers`: {shift, alt, command, ctrl}.",
        bridge_only: true,
        schema: || {
            schema(
                json!({
                    "id": {"type": "string"}, "x": {"type": "number"}, "y": {"type": "number"},
                    "button": {"type": "string", "enum": ["left", "right", "middle"]},
                    "count": {"type": "integer"}, "modifiers": {"type": "object"}
                }),
                &[],
            )
        },
        run: ui_click,
    },
    ToolDef {
        name: "ui_drag",
        description: "Press-move-release from `from` to `to`; each is {\"id\": element} (add fx/fy 0-1 for a spot inside it) or {\"x\",\"y\"}. Moves layers in the viewer, drags layer bars/keyframes/the CTI (`timeline.cti`), scrubs values, draws shapes with a shape tool.",
        bridge_only: true,
        schema: || {
            schema(
                json!({"from": {"type": "object"}, "to": {"type": "object"}, "steps": {"type": "integer"}, "modifiers": {"type": "object"}}),
                &["from", "to"],
            )
        },
        run: ui_drag,
    },
    ToolDef {
        name: "ui_key",
        description: "Press a key or shortcut: `Space` (play), `F9` (Easy Ease), `Cmd+Z`, `Cmd+N`, `PageDown`, `Escape`, `Enter` (`Cmd` = Ctrl off macOS).",
        bridge_only: true,
        schema: || schema(json!({"key": {"type": "string"}}), &["key"]),
        run: ui_key,
    },
    ToolDef {
        name: "ui_type",
        description: "Type text into the focused field (text layer editing, search boxes, value fields).",
        bridge_only: true,
        schema: || schema(json!({"text": {"type": "string"}}), &["text"]),
        run: ui_type,
    },
    ToolDef {
        name: "ui_set",
        description: "Set UI state directly: tool, workspace, theme (dark|darker|light), focused panel, viewer {zoom, res, pan, grid, rulers, safeMargins, transparencyGrid}, timeline {pps, start, graphEditor, showModes, openLayers, openGroups}, menuBar, home.",
        bridge_only: true,
        schema: || {
            schema(
                json!({
                    "tool": {"type": "string"}, "workspace": {"type": "string"}, "theme": {"type": "string"}, "focused": {"type": "string"},
                    "viewer": {"type": "object"}, "timeline": {"type": "object"}, "menuBar": {"type": "boolean"}, "home": {"type": "boolean"}
                }),
                &[],
            )
        },
        run: ui_set,
    },
    ToolDef {
        name: "control",
        description: "Call any control-channel method (docs/control-protocol.md) with its params, e.g. ui.menu.list, ui.menu.invoke {id}, ui.panel.show {panel}, ui.scroll, ui.viewer.locate {x,y}, ui.timeline.locate {layer,time}, ui.playback {action}, ui.resize.",
        bridge_only: true,
        schema: || schema(json!({"method": {"type": "string"}, "params": {"type": "object", "additionalProperties": true}}), &["method"]),
        run: control,
    },
    ToolDef {
        name: "batch",
        description: "Run several engine commands in one call as ONE undo step (engine.batch). Later steps can use earlier results: a string param \"$N\" or \"$N.key\" is step N's result (1-based), e.g. create a layer, then key and style it: {\"steps\":[{\"command\":\"layer.newText\",\"params\":{\"text\":\"Hi\",\"name\":\"Title\"}},{\"command\":\"prop.addKey\",\"params\":{\"layer\":\"$1.layer\",\"path\":\"transform/opacity\",\"time\":0,\"value\":0}},{\"command\":\"effect.apply\",\"params\":{\"layer\":\"$1.layer\",\"effect\":\"Glow\"}}]}. Returns {steps, results}. If a step fails the whole batch is rolled back (unless atomic: false) and the error names the step.",
        bridge_only: false,
        schema: || {
            schema(
                json!({
                    "steps": {"type": "array", "items": {"type": "object", "properties": {"command": {"type": "string"}, "params": {"type": "object", "additionalProperties": true}}, "required": ["command"]}},
                    "label": {"type": "string", "description": "Undo step name (default Batch)."},
                    "atomic": {"type": "boolean", "description": "Roll everything back when a step fails (default true)."}
                }),
                &["steps"],
            )
        },
        run: batch_tool,
    },
    ToolDef {
        name: "command_list",
        description: "List engine commands: id, label, menu path, shortcut, `params` doc (e.g. `{name?, width?, height?, frameRate?, duration? (s)}`) and whether it can run now (`why` if not). Every menu item, shortcut and panel gesture maps to one of these ids; run them with command_run. The full list is large (~130 KB): narrow with `filter` (e.g. `layer.new`, `keys`, `effect`) or pass `brief: true` for just ids and labels, then describe_command for one command's parameters.",
        bridge_only: false,
        schema: || {
            schema(
                json!({
                    "filter": {"type": "string", "description": "Only commands whose id or label contains this text (case-insensitive)."},
                    "enabled_only": {"type": "boolean", "description": "Only commands that can run right now."},
                    "schemas": {"type": "boolean", "description": "Also return each command's parameter JSON Schema (`schema`)."},
                    "brief": {"type": "boolean", "description": "Only {id, label} per command (~40 KB instead of ~125 KB); describe_command gives one command's params."}
                }),
                &[],
            )
        },
        run: list_commands,
    },
    ToolDef {
        name: "command_run",
        description: "Run any engine command by id with JSON params; edits are undoable. Examples: comp.new {\"name\":\"Main\",\"width\":1920,\"height\":1080,\"frameRate\":30,\"duration\":5}; layer.newText {\"text\":\"Hello\",\"size\":120,\"fill\":\"#ffffff\"} (returns {\"layer\":id}); layer.newSolid {\"color\":\"#3366ff\"}; layer.newShape; effect.apply {\"layer\":3,\"effect\":\"Gaussian Blur\"}; time.set {\"time\":2.5}; layer.select {\"layers\":[3]}. Layers are referenced by id, \"#n\" or name; times are seconds.",
        bridge_only: false,
        schema: || {
            schema(
                json!({
                    "id": {"type": "string", "description": "Command id, e.g. `comp.new`, `layer.newText`, `effect.apply` (see command_list)."},
                    "params": {"type": "object", "description": "Command parameters as documented by command_list.", "additionalProperties": true}
                }),
                &["id"],
            )
        },
        run: execute_command,
    },
    ToolDef {
        name: "render_preview",
        description: "Render a composition frame (default: the active comp at the current time, in comp seconds) and return it as a PNG image, longest side `max_side` (default 960, 0 = full size). `region` renders one rectangle at full resolution (detail checks); `layer` renders one layer solo; `item` renders a footage or solid item on its own; `overlays` draws layer bounds, anchor points, motion paths, safe margins or the grid on top and lists each layer's comp-space bounds. `path` also writes the PNG to disk; `inline: false` returns only the metadata. Use it to check your work; render_frames shows several times at once.",
        bridge_only: false,
        schema: || {
            schema(
                json!({
                    "comp": comp_s(),
                    "item": item_s(),
                    "layer": solo_s(),
                    "time": {"type": "number", "description": "Comp time in seconds (default: current time); source time for `item`."},
                    "max_side": {"type": "integer", "description": "Longest side in pixels (default 960; 0 = full size of the frame or region)."},
                    "region": region_s(),
                    "overlays": overlays_s(),
                    "path": {"type": "string", "description": "Also write the PNG here."},
                    "inline": {"type": "boolean", "description": "Include the image in the reply (default true)."},
                    "transparent": {"type": "boolean", "description": "Keep the frame's alpha (RGBA PNG, as an RGB + Alpha render writes it) instead of compositing over the comp's background colour."}
                }),
                &[],
            )
        },
        run: render_frame,
    },
    ToolDef {
        name: "command_batch",
        description: "Run commands in order, returning each result or error. Stop at the first error by default; stop_on_error: false continues. Each edit has its own undo step; use batch for an atomic undo group.",
        bridge_only: false,
        schema: || {
            schema(
                json!({"steps": {"type": "array", "items": {"type": "object", "properties": {"id": {"type": "string"}, "params": {"type": "object"}}, "required": ["id"], "additionalProperties": false}}, "stop_on_error": {"type": "boolean"}}),
                &["steps"],
            )
        },
        run: command_batch,
    },
    ToolDef {
        name: "doc_inspect",
        description: "Inspect the project overview and active composition (null when no composition is active), its layers as compact, paged rows like get_comp (`full`, `offset`, `limit` work the same).",
        bridge_only: false,
        schema: || schema(json!({"full": full_s("active comp layers"), "offset": offset_s(), "limit": limit_s("layers", crate::compact::COMP_LIMIT)}), &[]),
        run: doc_inspect,
    },
];

fn doc_inspect(b: &mut Backend, a: &Value) -> Result<Reply> {
    let project = b.exec("project.summary", json!({}))?;
    let comp = match b.exec("comp.info", json!({})) {
        Ok(info) => Some(crate::compact::comp_reply(info, a)?),
        Err(_) => None,
    };
    json_reply(json!({"project": project, "activeComp": comp}))
}

fn command_batch(b: &mut Backend, a: &Value) -> Result<Reply> {
    let steps = need(a, "steps")?.as_array().ok_or_else(|| Error::BadArgs("steps must be an array".into()))?;
    let stop = get(a, "stop_on_error").and_then(Value::as_bool).unwrap_or(true);
    let mut results = Vec::new();
    let (mut completed, mut failed) = (0, 0);
    for step in steps {
        match execute_command(b, step) {
            Ok(Reply::Json(result)) => {
                completed += 1;
                results.push(json!({"ok": true, "result": result}));
            }
            Ok(Reply::Image { .. }) => return Err(Error::Other("unexpected command image".into())),
            Err(e) => {
                failed += 1;
                results.push(json!({"ok": false, "error": e.to_string()}));
                if stop {
                    break;
                }
            }
        }
    }
    json_reply(json!({"completed": completed, "failed": failed, "results": results}))
}
