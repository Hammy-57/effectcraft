//! Compact, paged replies for the read tools (`get_comp`, `doc_inspect`, `get_property`,
//! `get_layer`): an agent sees the whole picture of a big comp within a small budget.
//!
//! Clients often cap a tool result (OpenMausBot: 8k characters). The engine's `comp.info` repeats
//! every default switch, blend mode and stretch per layer (~560 characters a layer), so a capped
//! client saw 5 of 110 layers and believed that was all. Rows here keep only what differs from
//! the defaults, and any list that is cut says so in-band (`truncated`), with the offset to
//! continue from. `full: true` keeps the engine's shape.

use serde_json::{Map, Value, json};

use crate::{Error, Result};

/// A requested window into a list.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Page {
    pub offset: usize,
    pub limit: usize,
}

/// The largest `limit` honoured (bigger values are clamped).
const MAX_LIMIT: usize = 10_000;

/// A non-negative integer argument (also as a numeric string, which some clients send).
fn count_arg(a: &Value, k: &str) -> Result<Option<usize>> {
    let bad = || Error::BadArgs(format!("`{k}` must be a non-negative integer"));
    match a.get(k) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(n)) => n.as_u64().map(|n| Some(usize::try_from(n).unwrap_or(usize::MAX))).ok_or_else(bad),
        Some(Value::String(s)) => s.trim().parse::<usize>().map(Some).map_err(|_| bad()),
        Some(_) => Err(bad()),
    }
}

impl Page {
    /// `offset` / `limit` from the tool arguments; `limit` defaults to `default_limit`.
    pub fn from_args(a: &Value, default_limit: usize) -> Result<Page> {
        let offset = count_arg(a, "offset")?.unwrap_or(0);
        let limit = count_arg(a, "limit")?.unwrap_or(default_limit);
        if limit == 0 {
            return Err(Error::BadArgs("`limit` must be at least 1".into()));
        }
        Ok(Page { offset, limit: limit.min(MAX_LIMIT) })
    }

    /// Whether the arguments ask for paging explicitly.
    pub fn requested(a: &Value) -> bool {
        ["offset", "limit"].iter().any(|k| a.get(*k).is_some_and(|v| !v.is_null()))
    }
}

/// Put `items[page]` into `out[field]`, with `out[count_key]` (the total), `returned` and
/// `next_offset` (null when nothing follows). A cut list also gets `truncated`.
pub(crate) fn put_page(out: &mut Value, field: &str, items: Vec<Value>, page: Page, count_key: &str) {
    let total = items.len();
    let start = page.offset.min(total);
    let end = start.saturating_add(page.limit).min(total);
    let slice: Vec<Value> = items.into_iter().skip(start).take(end - start).collect();
    let returned = slice.len();
    let next = (end < total).then_some(end);
    let Some(o) = out.as_object_mut() else { return };
    o.insert(field.into(), Value::Array(slice));
    o.insert(count_key.into(), json!(total));
    o.insert("returned".into(), json!(returned));
    o.insert("next_offset".into(), json!(next));
    if let Some(k) = next {
        o.insert(
            "truncated".into(),
            json!({"field": field, "offset": start, "returned": returned, "total": total, "next_offset": k, "hint": format!("{} of {total} {field} shown; call again with offset={k} for the rest", returned)}),
        );
    }
}

/// `x` rounded to 4 decimals (times and values: enough to tell frames apart at any rate).
fn round4(x: f64) -> Value {
    if !x.is_finite() || x.abs() >= 1e11 {
        return json!(x);
    }
    json!((x * 1e4).round() / 1e4)
}

/// Numbers in `v` (also inside arrays) rounded to 4 decimals; anything else unchanged.
pub(crate) fn round_value(v: &Value) -> Value {
    match v {
        Value::Number(n) if !n.is_i64() && !n.is_u64() => n.as_f64().map(round4).unwrap_or_else(|| v.clone()),
        Value::Array(a) => Value::Array(a.iter().map(round_value).collect()),
        _ => v.clone(),
    }
}

/// A layer switch: its key in `comp.info`'s `switches`, default, and how a differing value reads.
enum Switch {
    /// Shown as `on` when true (default false).
    On(&'static str, &'static str),
    /// Shown as `off` when false (default true).
    Off(&'static str, &'static str),
    /// An option, shown as `label:Value` when not the default.
    Opt(&'static str, &'static str, &'static str),
}

/// Every switch of `effectcraft_project::Switches` (a test checks none is missing).
const SWITCHES: &[Switch] = &[
    Switch::Off("video", "hidden"),
    Switch::Off("audio", "audio_off"),
    Switch::Off("effects", "fx_off"),
    Switch::On("solo", "solo"),
    Switch::On("locked", "locked"),
    Switch::On("shy", "shy"),
    Switch::On("collapse", "collapse"),
    Switch::On("motion_blur", "motion_blur"),
    Switch::On("adjustment", "adjustment"),
    Switch::On("three_d", "3d"),
    Switch::On("guide", "guide"),
    Switch::Opt("quality", "Best", "quality"),
    Switch::Opt("sampling", "Bilinear", "sampling"),
    Switch::Opt("frame_blend", "Off", "frame_blend"),
];

/// The switch keys this module knows.
#[cfg(test)]
pub(crate) fn known_switches() -> Vec<&'static str> {
    SWITCHES
        .iter()
        .map(|s| match s {
            Switch::On(k, _) | Switch::Off(k, _) | Switch::Opt(k, _, _) => *k,
        })
        .collect()
}

/// The switches that differ from the defaults, as short tags (`["3d", "hidden", "quality:Draft"]`).
fn switch_tags(sw: &Value) -> Vec<Value> {
    let Some(m) = sw.as_object() else { return vec![] };
    let mut tags = vec![];
    for s in SWITCHES {
        match s {
            Switch::On(k, tag) if m.get(*k).and_then(Value::as_bool) == Some(true) => tags.push(json!(tag)),
            Switch::Off(k, tag) if m.get(*k).and_then(Value::as_bool) == Some(false) => tags.push(json!(tag)),
            Switch::Opt(k, default, tag) => {
                if let Some(v) = m.get(*k).and_then(Value::as_str).filter(|v| v != default) {
                    tags.push(json!(format!("{tag}:{v}")));
                }
            }
            _ => {}
        }
    }
    // A switch added to the engine later: shown with its value rather than lost.
    let known = |k: &str| SWITCHES.iter().any(|s| matches!(s, Switch::On(n, _) | Switch::Off(n, _) | Switch::Opt(n, _, _) if *n == k));
    for (k, v) in m.iter().filter(|(k, _)| !known(k)) {
        tags.push(json!(format!("{k}:{v}")));
    }
    tags
}

/// One `comp.info` layer as a compact row: index, id, name, type, in, out, then only what is not
/// the default (start ≠ 0, stretch ≠ 100, parent, trackMatte, blend mode ≠ Normal, switches).
pub(crate) fn layer_row(l: &Value) -> Value {
    let mut o = Map::new();
    for k in ["index", "id", "name", "type"] {
        if let Some(v) = l.get(k) {
            o.insert(k.into(), v.clone());
        }
    }
    for k in ["in", "out"] {
        if let Some(v) = l.get(k) {
            o.insert(k.into(), round_value(v));
        }
    }
    if let Some(s) = l.get("start").and_then(Value::as_f64).filter(|s| *s != 0.0) {
        o.insert("start".into(), round4(s));
    }
    if let Some(s) = l.get("stretch").and_then(Value::as_f64).filter(|s| (*s - 100.0).abs() > 1e-9) {
        o.insert("stretch".into(), round4(s));
    }
    for k in ["parent", "trackMatte"] {
        if let Some(v) = l.get(k).filter(|v| !v.is_null()) {
            o.insert(k.into(), v.clone());
        }
    }
    if let Some(b) = l.get("blendMode").and_then(Value::as_str).filter(|b| *b != "Normal") {
        o.insert("blendMode".into(), json!(b));
    }
    if let Some(sw) = l.get("switches") {
        let tags = switch_tags(sw);
        if !tags.is_empty() {
            o.insert("switches".into(), Value::Array(tags));
        }
    }
    Value::Object(o)
}

/// The default number of layers per `get_comp` page: ~300 compact layers still fit one call.
pub(crate) const COMP_LIMIT: usize = 200;
/// The default number of keyframes per `get_property` page.
pub(crate) const KEY_LIMIT: usize = 100;
/// The default number of properties per `get_layer {flat: true}` page.
pub(crate) const FLAT_LIMIT: usize = 150;
/// A `get_layer` tree bigger than this (characters) carries a hint to page the flat list.
pub(crate) const TREE_HINT_CHARS: usize = 8_000;

/// What compact rows leave out, said once per reply.
const COMP_NOTE: &str = "Compact rows: omitted start = 0, stretch = 100, blendMode = Normal, switches = defaults (tags list the differing ones: hidden = video off, 3d, solo, locked, shy, adjustment, ...). Pass full: true for every field.";

/// A `comp.info` reply shaped for an agent: compact rows unless `full`, paged when asked (and by
/// default when compact). `full` without `offset`/`limit` is `comp.info` unchanged.
pub(crate) fn comp_reply(mut info: Value, a: &Value) -> Result<Value> {
    let full = a.get("full").and_then(Value::as_bool) == Some(true);
    if full && !Page::requested(a) {
        return Ok(info);
    }
    let page = Page::from_args(a, COMP_LIMIT)?;
    let layers = match info.get_mut("layers").map(Value::take) {
        Some(Value::Array(l)) => l,
        _ => vec![],
    };
    let rows = if full { layers } else { layers.iter().map(layer_row).collect() };
    put_page(&mut info, "layers", rows, page, "layerCount");
    if !full && let Some(o) = info.as_object_mut() {
        o.insert("compact".into(), json!(COMP_NOTE));
    }
    Ok(info)
}

/// One `prop.get` keyframe, compact: time (layer s), value (rounded), `compTime` only when it
/// differs from `time`, `in`/`out` only when not Linear, and any warning.
fn key_row(k: &Value) -> Value {
    let mut o = Map::new();
    if let Some(t) = k.get("time") {
        o.insert("time".into(), round_value(t));
    }
    if let Some(ct) = k.get("compTime").filter(|ct| k.get("time").is_none_or(|t| round_value(t) != round_value(ct))) {
        o.insert("compTime".into(), round_value(ct));
    }
    if let Some(v) = k.get("value") {
        o.insert("value".into(), round_value(v));
    }
    for i in ["in", "out"] {
        if let Some(v) = k.get(i).filter(|v| v.as_str() != Some("Linear")) {
            o.insert(i.into(), v.clone());
        }
    }
    if let Some(w) = k.get("warning") {
        o.insert("warning".into(), w.clone());
    }
    Value::Object(o)
}

/// A `prop.get` reply with its keyframes compact (unless `full`) and paged: `keyCount`,
/// `returned`, `next_offset`. `full` without `offset`/`limit` is `prop.get` unchanged.
pub(crate) fn property_reply(mut r: Value, a: &Value) -> Result<Value> {
    let full = a.get("full").and_then(Value::as_bool) == Some(true);
    if full && !Page::requested(a) {
        return Ok(r);
    }
    let page = Page::from_args(a, KEY_LIMIT)?;
    let keys = match r.get_mut("keys").map(Value::take) {
        Some(Value::Array(k)) => k,
        _ => vec![],
    };
    let rows = if full { keys } else { keys.iter().map(key_row).collect() };
    put_page(&mut r, "keys", rows, page, "keyCount");
    Ok(r)
}

/// A `get_layer {flat: true}` list, paged: `total`, `returned`, `next_offset`.
pub(crate) fn flat_reply(r: &mut Value, props: Vec<Value>, a: &Value) -> Result<()> {
    let page = Page::from_args(a, FLAT_LIMIT)?;
    put_page(r, "properties", props, page, "total");
    Ok(())
}

/// A hint on a `get_layer` tree too big for capped clients.
pub(crate) fn tree_hint(r: &mut Value) {
    let size = r.to_string().len();
    if size > TREE_HINT_CHARS
        && let Some(o) = r.as_object_mut()
    {
        o.insert(
            "hint".into(),
            json!(format!(
                "This tree is {size} characters. For a smaller reply use flat: true (paged with offset/limit), depth: 1, or get_property on one path."
            )),
        );
    }
}
