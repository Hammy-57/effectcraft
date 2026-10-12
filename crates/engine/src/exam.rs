//! The agent exam's checker (`exam.check`, `evals/agent/`): deterministic checks on a finished
//! project. A check runs one read-only query command and tests facts of its JSON result with JSON
//! Pointers (RFC 6901), optionally after selecting an array element by its fields. Shorthands
//! (`lint`, `motion`, `compare`, `prop`, `layerCount`, `comp`) expand into that generic form.
//!
//! This module is pure: it parses and expands checks and evaluates expectations on a result. The
//! command (`commands/exam_cmds.rs`) runs the queries. A malformed check is a failed result with a
//! reason, never an error of the whole run. Format: `evals/agent/README.md`.

use serde_json::{Map, Value, json};

/// Most checks one `exam.check` runs.
pub const MAX_CHECKS: usize = 200;
/// Most expectations in one check (or one `any` / `all` / `none` element test).
pub const MAX_EXPECT: usize = 64;
/// Most selection steps in one `select` chain.
pub const MAX_SELECT: usize = 8;
/// Deepest nesting of `any` / `all` / `none`.
const MAX_DEPTH: usize = 6;
/// Default tolerance of `approx`.
pub const DEFAULT_TOL: f64 = 1e-3;
/// Longest JSON quoted in a reason.
const QUOTE_MAX: usize = 160;

/// One check in generic form: run `query` with `params`, select, then test each expectation.
#[derive(Clone, Debug, PartialEq)]
pub struct Check {
    pub id: String,
    pub desc: String,
    pub query: String,
    pub params: Value,
    /// Selection steps applied to the query result before the expectation paths.
    pub select: Vec<Value>,
    /// Expectations (`{path, op, value?, tol?, length?, select?}`), checked when evaluated.
    pub expect: Vec<Value>,
    /// JSON Pointer (into the query result) of the value reported as `actual` when the check fails.
    pub show: Option<String>,
}

impl Check {
    /// The generic JSON form of this check (what a shorthand expands to).
    pub fn to_json(&self) -> Value {
        let mut o = json!({"id": self.id, "desc": self.desc, "query": self.query, "params": self.params, "expect": self.expect});
        if !self.select.is_empty() {
            o["select"] = json!(self.select);
        }
        if let Some(s) = &self.show {
            o["show"] = json!(s);
        }
        o
    }
}

/// The outcome of evaluating a check's expectations on a query result.
#[derive(Clone, Debug, PartialEq)]
pub struct Outcome {
    pub pass: bool,
    /// Plain English: what was expected and what was found (empty when it passed).
    pub reason: String,
    /// The value the failing expectation saw (or the check's `show` value).
    pub actual: Value,
    /// The check itself is wrong (an unknown op, a missing value…), not the project.
    pub malformed: bool,
}

impl Outcome {
    fn ok() -> Outcome {
        Outcome { pass: true, reason: String::new(), actual: Value::Null, malformed: false }
    }
    fn fail(reason: impl Into<String>, actual: Value) -> Outcome {
        Outcome { pass: false, reason: reason.into(), actual, malformed: false }
    }
    /// A failure caused by a malformed check.
    fn bad(reason: impl Into<String>) -> Outcome {
        Outcome { pass: false, reason: reason.into(), actual: Value::Null, malformed: true }
    }
}

/// The id of check `i` (0-based): its `id`, else `check<i+1>`.
pub fn check_id(c: &Value, i: usize) -> String {
    match c.get("id") {
        Some(Value::String(s)) if !s.trim().is_empty() => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => format!("check{}", i.saturating_add(1)),
    }
}

/// The description of a check (`desc`, else empty).
pub fn check_desc(c: &Value) -> String {
    c.get("desc").and_then(Value::as_str).unwrap_or_default().to_string()
}

/// Expand check `i` (generic or a shorthand) into its generic form. `base`: the folder relative
/// reference paths (`compare`) resolve against.
pub fn expand(c: &Value, i: usize, base: Option<&str>) -> Result<Check, String> {
    let o = c.as_object().ok_or_else(|| format!("a check must be an object, got {}", quote(c)))?;
    let id = check_id(c, i);
    let desc = check_desc(c);
    let shorthands = ["query", "lint", "motion", "compare", "prop", "layerCount", "comp"];
    let kinds: Vec<&str> = shorthands.iter().copied().filter(|k| o.contains_key(*k)).collect();
    let kind = match kinds.as_slice() {
        [k] => *k,
        [] => return Err(format!("a check needs one of {} (see evals/agent/README.md)", shorthands.map(|k| format!("`{k}`")).join(", "))),
        many => return Err(format!("a check takes one kind, got {}", many.iter().map(|k| format!("`{k}`")).collect::<Vec<_>>().join(" and "))),
    };
    let allowed: &[&str] = match kind {
        "query" => &["query", "params", "select", "expect", "show"],
        "lint" => &["lint", "maxSeverity", "allow"],
        "motion" => &["motion", "layer", "segment"],
        "compare" => &["compare"],
        "prop" | "layerCount" => &[kind, "op", "value", "tol"],
        _ => &["comp", "width", "height", "fps", "duration", "tol"],
    };
    if let Some(k) = o.keys().find(|k| !["id", "desc"].contains(&k.as_str()) && !allowed.contains(&k.as_str())) {
        return Err(format!("unknown field `{k}` for a `{kind}` check (id, desc, {})", allowed.join(", ")));
    }
    let mut chk = match kind {
        "query" => generic(o)?,
        "lint" => lint(o)?,
        "motion" => motion(o)?,
        "compare" => compare(o, base)?,
        "prop" => prop(o)?,
        "layerCount" => layer_count(o)?,
        _ => comp(o)?,
    };
    chk.id = id;
    chk.desc = desc;
    if chk.expect.is_empty() {
        return Err("the check has no expectations (`expect` is empty)".into());
    }
    if chk.expect.len() > MAX_EXPECT {
        return Err(format!("too many expectations ({}; at most {MAX_EXPECT})", chk.expect.len()));
    }
    if chk.select.len() > MAX_SELECT {
        return Err(format!("too many selection steps ({}; at most {MAX_SELECT})", chk.select.len()));
    }
    Ok(chk)
}

fn params_of(o: &Map<String, Value>, key: &str) -> Result<Value, String> {
    match o.get(key) {
        None | Some(Value::Null) => Ok(json!({})),
        Some(v @ Value::Object(_)) => Ok(v.clone()),
        Some(v) => Err(format!("`{key}` must be an object of query parameters, got {}", quote(v))),
    }
}

/// `select`: one step (object) or a chain (array of steps).
fn select_of(v: Option<&Value>) -> Result<Vec<Value>, String> {
    match v {
        None | Some(Value::Null) => Ok(vec![]),
        Some(s @ Value::Object(_)) => Ok(vec![s.clone()]),
        Some(Value::Array(a)) => Ok(a.clone()),
        Some(v) => Err(format!("`select` must be an object or an array of them, got {}", quote(v))),
    }
}

fn generic(o: &Map<String, Value>) -> Result<Check, String> {
    let query = o.get("query").and_then(Value::as_str).map(str::trim).filter(|q| !q.is_empty()).ok_or("`query` must be a command id")?;
    let expect = match o.get("expect") {
        Some(Value::Array(a)) => a.clone(),
        Some(e @ Value::Object(_)) => vec![e.clone()],
        Some(v) => return Err(format!("`expect` must be an array of expectations, got {}", quote(v))),
        None => return Err("missing `expect` (an array of {path, op, value})".into()),
    };
    let show = match o.get("show") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => Some(s.clone()),
        Some(v) => return Err(format!("`show` must be a JSON Pointer, got {}", quote(v))),
    };
    Ok(Check {
        id: String::new(),
        desc: String::new(),
        query: query.to_string(),
        params: params_of(o, "params")?,
        select: select_of(o.get("select"))?,
        expect,
        show,
    })
}

fn check(query: &str, params: Value, select: Vec<Value>, expect: Vec<Value>, show: Option<&str>) -> Check {
    Check { id: String::new(), desc: String::new(), query: query.into(), params, select, expect, show: show.map(str::to_string) }
}

/// `{lint: {params}, maxSeverity?: info|warning|none, allow?: [rule]}`: `render.lint` finds no
/// problem worse than `maxSeverity` (default `info`: no errors or warnings, i.e. clean), ignoring
/// the rules in `allow`.
fn lint(o: &Map<String, Value>) -> Result<Check, String> {
    let params = params_of(o, "lint")?;
    let above: &[&str] = match o.get("maxSeverity").and_then(Value::as_str).unwrap_or("info") {
        "info" => &["error", "warning"],
        "warning" => &["error"],
        "none" => &["error", "warning", "info"],
        other => return Err(format!("`maxSeverity` must be info, warning or none, got {}", quote(&json!(other)))),
    };
    let mut element = vec![json!({"path": "/severity", "op": "in", "value": above})];
    match o.get("allow") {
        None | Some(Value::Null) => {}
        Some(a @ Value::Array(_)) => element.push(json!({"path": "/rule", "op": "notIn", "value": a})),
        Some(v) => return Err(format!("`allow` must be an array of rule names, got {}", quote(v))),
    }
    Ok(check("render.lint", params, vec![], vec![json!({"path": "/problems", "op": "none", "value": element})], None))
}

/// `{motion: {params}, layer, segment: {kind, edge?, direction?, startApprox?, endApprox?, tol?}
/// | [..]}`: `comp.motion` reports a segment with those facts for `layer` (each one, for a list).
fn motion(o: &Map<String, Value>) -> Result<Check, String> {
    let mut params = params_of(o, "motion")?;
    let layer = o.get("layer").filter(|v| !v.is_null()).ok_or("`motion` checks need `layer` (a name, id or \"#n\")")?;
    let step = match layer {
        Value::String(s) if s.starts_with('#') => json!({"array": "/layers", "index": 0}),
        Value::String(s) => json!({"array": "/layers", "where": {"name": s}}),
        Value::Number(n) => json!({"array": "/layers", "where": {"layer": n}}),
        v => return Err(format!("`layer` must be a name, id or \"#n\", got {}", quote(v))),
    };
    if params.get("layers").is_none() && params.get("layer").is_none() {
        params["layers"] = json!([layer]);
    }
    let segs = match o.get("segment") {
        Some(s @ Value::Object(_)) => vec![s.clone()],
        Some(Value::Array(a)) if !a.is_empty() => a.clone(),
        Some(v) => return Err(format!("`segment` must be an object (or a list of them), got {}", quote(v))),
        None => return Err("`motion` checks need `segment` ({kind, edge?, direction?, startApprox?, endApprox?, tol?})".into()),
    };
    let mut expect = vec![];
    for seg in &segs {
        let so = seg.as_object().ok_or_else(|| format!("a segment must be an object, got {}", quote(seg)))?;
        let tol = match so.get("tol") {
            None => 0.1,
            Some(t) => t.as_f64().filter(|t| t.is_finite() && *t >= 0.0).ok_or_else(|| format!("segment `tol` must be a number ≥ 0, got {}", quote(t)))?,
        };
        if let Some(k) = so.keys().find(|k| !["kind", "edge", "direction", "startApprox", "endApprox", "tol"].contains(&k.as_str())) {
            return Err(format!("unknown segment field `{k}` (kind, edge, direction, startApprox, endApprox, tol)"));
        }
        if !so.contains_key("kind") {
            return Err("a segment needs `kind` (enter, exit, move, hold, scale, rotate, fade-in, fade-out)".into());
        }
        // In a fixed order, so the expansion (and its reasons) read the same every time.
        let mut element = vec![];
        for k in ["kind", "edge", "direction"] {
            if let Some(v) = so.get(k) {
                element.push(json!({"path": format!("/{k}"), "op": "eq", "value": v}));
            }
        }
        for (k, path) in [("startApprox", "/start"), ("endApprox", "/end")] {
            if let Some(v) = so.get(k) {
                element.push(json!({"path": path, "op": "approx", "value": v, "tol": tol}));
            }
        }
        expect.push(json!({"path": "/segments", "op": "any", "value": element}));
    }
    Ok(check("comp.motion", params, vec![step], expect, Some("/layers")))
}

/// `{compare: {reference, comp?, time?, threshold?, minPsnr?, maxSide?, maxChangedPct?}}`: the
/// rendered frame matches a reference image (`render.compare`). Without `minPsnr` /
/// `maxChangedPct` every pixel must be within `threshold` levels.
fn compare(o: &Map<String, Value>, base: Option<&str>) -> Result<Check, String> {
    let c = o.get("compare").and_then(Value::as_object).ok_or("`compare` must be an object {reference, comp?, time?, threshold?}")?;
    let reference = c.get("reference").and_then(Value::as_str).filter(|r| !r.is_empty()).ok_or("`compare` needs `reference` (an image path)")?;
    let path = match base.filter(|b| !b.is_empty()) {
        Some(b) if std::path::Path::new(reference).is_relative() => std::path::Path::new(b).join(reference).to_string_lossy().into_owned(),
        _ => reference.to_string(),
    };
    let mut a = json!({});
    for k in ["comp", "time"] {
        if let Some(v) = c.get(k).filter(|v| !v.is_null()) {
            a[k] = v.clone();
        }
    }
    let mut params = json!({"a": a, "b": {"path": path}});
    for (k, p) in [("threshold", "threshold"), ("maxSide", "max_side"), ("minPsnr", "min_psnr")] {
        if let Some(v) = c.get(k).filter(|v| !v.is_null()) {
            params[p] = v.clone();
        }
    }
    for k in c.keys() {
        if !["reference", "comp", "time", "threshold", "maxSide", "minPsnr", "maxChangedPct"].contains(&k.as_str()) {
            return Err(format!("unknown `compare` field `{k}` (reference, comp, time, threshold, maxSide, minPsnr, maxChangedPct)"));
        }
    }
    let expect = match c.get("maxChangedPct") {
        Some(m) => vec![json!({"path": "/changed_pct", "op": "lte", "value": m})],
        None => vec![json!({"path": "/pass", "op": "eq", "value": true})],
    };
    Ok(check("render.compare", params, vec![], expect, None))
}

/// The `op` / `value` / `tol` of a shorthand, as one expectation on `path`.
fn op_expect(o: &Map<String, Value>, path: Value, length: bool) -> Result<Value, String> {
    let op = o.get("op").and_then(Value::as_str).ok_or("missing `op` (eq, ne, approx, gt, gte, lt, lte, …)")?;
    let mut e = json!({"path": path, "op": op});
    for k in ["value", "tol"] {
        if let Some(v) = o.get(k) {
            e[k] = v.clone();
        }
    }
    if length {
        e["length"] = json!(true);
    }
    Ok(e)
}

/// `{prop: {comp?, layer, path, time?, field?}, op, value, tol?}`: a property (`prop.get`). The
/// value tested is what renders: the expression result when there is one, else the value at
/// `time` (comp seconds, default the current time). `field` picks another part of the reply
/// (`keys`, `expression`, `animated`).
fn prop(o: &Map<String, Value>) -> Result<Check, String> {
    let p = o.get("prop").and_then(Value::as_object).ok_or("`prop` must be an object {comp?, layer, path, time?}")?;
    if let Some(k) = p.keys().find(|k| !["comp", "layer", "path", "time", "field"].contains(&k.as_str())) {
        return Err(format!("unknown `prop` field `{k}` (comp, layer, path, time, field)"));
    }
    let mut params = json!({});
    for k in ["comp", "layer", "path", "time"] {
        if let Some(v) = p.get(k).filter(|v| !v.is_null()) {
            params[k] = v.clone();
        }
    }
    if params.get("layer").is_none() || params.get("path").is_none() {
        return Err("`prop` needs `layer` and `path` (a property path such as transform/position)".into());
    }
    let path = match p.get("field").and_then(Value::as_str) {
        Some(f) => json!(format!("/{}", f.trim_start_matches('/'))),
        None => json!(["/evaluated", "/value"]),
    };
    Ok(check("prop.get", params, vec![], vec![op_expect(o, path, false)?], None))
}

/// `{layerCount: {comp?}, op, value}`: how many layers a comp has (`comp.info`).
fn layer_count(o: &Map<String, Value>) -> Result<Check, String> {
    let params = params_of(o, "layerCount")?;
    Ok(check("comp.info", params, vec![], vec![op_expect(o, json!("/layers"), true)?], Some("/layers")))
}

/// `{comp: {name}, width?, height?, fps?, duration?, tol?}`: a comp with that name and those
/// settings exists (`comp.info`). `tol` (default 0.01) applies to fps and duration.
fn comp(o: &Map<String, Value>) -> Result<Check, String> {
    let c = o.get("comp").and_then(Value::as_object).ok_or("`comp` must be an object {name}")?;
    let name = c.get("name").filter(|n| n.is_string() || n.is_number()).ok_or("`comp` needs `name` (or an id)")?;
    let tol = o.get("tol").and_then(Value::as_f64).unwrap_or(0.01);
    let mut expect = vec![];
    for (k, path, approx) in [("width", "/width", false), ("height", "/height", false), ("fps", "/frameRate", true), ("duration", "/duration", true)] {
        if let Some(v) = o.get(k) {
            expect.push(if approx { json!({"path": path, "op": "approx", "value": v, "tol": tol}) } else { json!({"path": path, "op": "eq", "value": v}) });
        }
    }
    if expect.is_empty() {
        // Just that it exists.
        expect.push(json!({"path": "/id", "op": "exists"}));
    }
    Ok(check("comp.info", json!({"comp": name}), vec![], expect, None))
}

// ---------------------------------------------------------------- evaluation

/// Evaluate a check's selection and expectations on its query's `result`.
pub fn evaluate(c: &Check, result: &Value) -> Outcome {
    let root = match select_chain(result, &c.select) {
        Ok(r) => r,
        Err(why) => return Outcome::fail(why, show(c, result)),
    };
    for e in &c.expect {
        let o = expectation(root, e, 0);
        if !o.pass {
            let actual = if c.show.is_some() { show(c, result) } else { o.actual };
            return Outcome { actual, ..o };
        }
    }
    Outcome::ok()
}

fn show(c: &Check, result: &Value) -> Value {
    match &c.show {
        Some(p) => truncate(result.pointer(p).cloned().unwrap_or(Value::Null)),
        None => Value::Null,
    }
}

/// Keep reported values small: long arrays are cut to their first 20 elements.
fn truncate(v: Value) -> Value {
    match v {
        Value::Array(a) if a.len() > 20 => {
            let n = a.len();
            let mut out: Vec<Value> = a.into_iter().take(20).collect();
            out.push(json!(format!("… {} more", n.saturating_sub(20))));
            Value::Array(out)
        }
        v => v,
    }
}

/// Apply selection steps: each `{array: pointer, where?: {field|/pointer: value}, index?: n}`
/// picks the `index`-th (0-based, default first) element of the array that matches `where`.
pub fn select_chain<'a>(v: &'a Value, steps: &[Value]) -> Result<&'a Value, String> {
    let mut cur = v;
    for step in steps.iter().take(MAX_SELECT) {
        cur = select_one(cur, step)?;
    }
    Ok(cur)
}

fn select_one<'a>(v: &'a Value, step: &Value) -> Result<&'a Value, String> {
    let o = step.as_object().ok_or_else(|| format!("a `select` step must be an object {{array, where?, index?}}, got {}", quote(step)))?;
    for k in o.keys() {
        if !["array", "where", "index"].contains(&k.as_str()) {
            return Err(format!("unknown `select` field `{k}` (array, where, index)"));
        }
    }
    let ptr = match o.get("array") {
        None => "",
        Some(Value::String(p)) => p.as_str(),
        Some(p) => return Err(format!("`select.array` must be a JSON Pointer, got {}", quote(p))),
    };
    let arr = v.pointer(ptr).ok_or_else(|| format!("{}: not found in the result", label(ptr)))?;
    let arr = arr.as_array().ok_or_else(|| format!("{}: expected an array to select from, got {}", label(ptr), quote(arr)))?;
    let wh = match o.get("where") {
        None | Some(Value::Null) => None,
        Some(Value::Object(w)) => Some(w),
        Some(w) => return Err(format!("`select.where` must be an object of field values, got {}", quote(w))),
    };
    let index = match o.get("index") {
        None => 0,
        Some(i) => i.as_u64().and_then(|i| usize::try_from(i).ok()).ok_or_else(|| format!("`select.index` must be a whole number ≥ 0, got {}", quote(i)))?,
    };
    let matching = |e: &Value| wh.is_none_or(|w| w.iter().all(|(k, want)| field(e, k).is_some_and(|got| json_eq(got, want))));
    arr.iter().filter(|e| matching(e)).nth(index).ok_or_else(|| {
        let what = match wh {
            Some(w) => format!("no element of {} with {}", label(ptr), w.iter().map(|(k, v)| format!("{k} = {}", quote(v))).collect::<Vec<_>>().join(", ")),
            None => format!("{} has no element {index}", label(ptr)),
        };
        let names: Vec<String> = arr.iter().filter_map(|e| e.get("name").and_then(Value::as_str)).take(20).map(|n| format!("{n:?}")).collect();
        if names.is_empty() { format!("{what} ({} elements)", arr.len()) } else { format!("{what} (names: {})", names.join(", ")) }
    })
}

/// `key` of an element: a JSON Pointer when it starts with `/`, else a top-level field.
fn field<'a>(e: &'a Value, key: &str) -> Option<&'a Value> {
    if key.starts_with('/') { e.pointer(key) } else { e.get(key) }
}

fn label(ptr: &str) -> String {
    if ptr.is_empty() { "the result".to_string() } else { ptr.to_string() }
}

/// A short JSON rendering for reasons.
pub fn quote(v: &Value) -> String {
    let s = v.to_string();
    if s.chars().count() <= QUOTE_MAX {
        return s;
    }
    let cut: String = s.chars().take(QUOTE_MAX).collect();
    format!("{cut}…")
}

/// Equality with numbers compared by value (`1` = `1.0`, within 1e-9) and arrays / objects
/// compared element by element.
pub fn json_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => match (x.as_f64(), y.as_f64()) {
            (Some(x), Some(y)) => (x - y).abs() <= 1e-9 * x.abs().max(y.abs()).max(1.0),
            _ => x == y,
        },
        (Value::Array(x), Value::Array(y)) => x.len() == y.len() && x.iter().zip(y).all(|(a, b)| json_eq(a, b)),
        (Value::Object(x), Value::Object(y)) => x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| json_eq(v, w))),
        _ => a == b,
    }
}

/// `contains` on an array element: objects match when they hold every field of `want`, strings
/// case-insensitively, anything else by [`json_eq`].
fn element_matches(e: &Value, want: &Value) -> bool {
    match (e, want) {
        (Value::Object(eo), Value::Object(wo)) => wo.iter().all(|(k, w)| eo.get(k).is_some_and(|v| element_matches(v, w))),
        (Value::String(a), Value::String(b)) => a.to_lowercase() == b.to_lowercase(),
        _ => json_eq(e, want),
    }
}

/// Case-insensitive glob: `*` any run of characters, `?` one character.
pub fn glob(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.to_lowercase().chars().collect();
    let t: Vec<char> = text.to_lowercase().chars().collect();
    // Iterative matcher with one backtrack point (linear in practice, no recursion).
    let (mut pi, mut ti, mut star, mut mark) = (0usize, 0usize, None::<usize>, 0usize);
    while ti < t.len() {
        match p.get(pi) {
            Some('*') => {
                star = Some(pi);
                mark = ti;
                pi += 1;
            }
            Some(c) if *c == '?' || Some(c) == t.get(ti) => {
                pi += 1;
                ti += 1;
            }
            _ => match star {
                Some(s) => {
                    pi = s + 1;
                    mark += 1;
                    ti = mark;
                }
                None => return false,
            },
        }
    }
    p.get(pi..).is_some_and(|rest| rest.iter().all(|c| *c == '*'))
}

fn length_of(v: &Value) -> Option<usize> {
    match v {
        Value::Array(a) => Some(a.len()),
        Value::String(s) => Some(s.chars().count()),
        Value::Object(o) => Some(o.len()),
        _ => None,
    }
}

/// The value at `path` (a pointer or a list of alternatives: the first that is present and not
/// null) and the pointer that found it.
fn resolve<'a>(root: &'a Value, path: Option<&Value>) -> Result<(Option<&'a Value>, String), String> {
    match path {
        None | Some(Value::Null) => Ok((Some(root), String::new())),
        Some(Value::String(p)) => Ok((root.pointer(p).filter(|v| !v.is_null()), p.clone())),
        Some(Value::Array(alts)) if !alts.is_empty() => {
            let mut ptrs = vec![];
            for a in alts {
                let p = a.as_str().ok_or_else(|| format!("`path` alternatives must be JSON Pointers, got {}", quote(a)))?;
                if let Some(v) = root.pointer(p).filter(|v| !v.is_null()) {
                    return Ok((Some(v), p.to_string()));
                }
                ptrs.push(p.to_string());
            }
            Ok((None, ptrs.join(" or ")))
        }
        Some(p) => Err(format!("`path` must be a JSON Pointer such as /layers/0/name, got {}", quote(p))),
    }
}

/// The ops an expectation can use.
pub const OPS: &[&str] = &[
    "eq", "ne", "approx", "gt", "gte", "lt", "lte", "contains", "matches", "exists", "absent", "len", "lenGte", "lenLte", "in", "notIn", "any", "all", "none",
];

/// "kind = \"enter\"": an expectation in words.
fn describe(e: &Value) -> String {
    let path = match e.get("path") {
        Some(Value::String(p)) => label(p),
        Some(Value::Array(a)) => a.iter().filter_map(Value::as_str).map(label).collect::<Vec<_>>().join(" or "),
        _ => "the value".to_string(),
    };
    let op = e.get("op").and_then(Value::as_str).unwrap_or("?");
    let v = e.get("value").map(quote).unwrap_or_default();
    let len = if e.get("length").and_then(Value::as_bool) == Some(true) { "length of " } else { "" };
    match op {
        "exists" | "absent" => format!("{path} {op}"),
        "approx" => format!("{len}{path} ≈ {v} ±{}", e.get("tol").and_then(Value::as_f64).unwrap_or(DEFAULT_TOL)),
        "any" | "all" | "none" => format!("{op} element of {path} matching ({})", element_words(e.get("value"))),
        _ => format!("{len}{path} {} {v}", op_words(op)),
    }
}

fn element_words(v: Option<&Value>) -> String {
    match v {
        Some(Value::Array(a)) => a.iter().map(describe).collect::<Vec<_>>().join(", "),
        Some(e) => describe(e),
        None => String::new(),
    }
}

fn op_words(op: &str) -> &str {
    match op {
        "eq" => "=",
        "ne" => "≠",
        "gt" => ">",
        "gte" => "≥",
        "lt" => "<",
        "lte" => "≤",
        "len" => "has length",
        "lenGte" => "has length ≥",
        "lenLte" => "has length ≤",
        "in" => "is one of",
        "notIn" => "is not one of",
        other => other,
    }
}

/// Evaluate one expectation on `root`.
pub fn expectation(root: &Value, e: &Value, depth: usize) -> Outcome {
    let Some(eo) = e.as_object() else { return Outcome::bad(format!("an expectation must be an object {{path, op, value}}, got {}", quote(e))) };
    for k in eo.keys() {
        if !["path", "op", "value", "tol", "length", "select"].contains(&k.as_str()) {
            return Outcome::bad(format!("unknown expectation field `{k}` (path, op, value, tol, length, select)"));
        }
    }
    let Some(op) = eo.get("op").and_then(Value::as_str) else {
        return Outcome::bad(format!("an expectation needs `op` (one of {})", OPS.join(", ")));
    };
    if !OPS.contains(&op) {
        return Outcome::bad(format!("unknown op `{op}` (one of {})", OPS.join(", ")));
    }
    let steps = match select_of(eo.get("select")) {
        Ok(s) => s,
        Err(why) => return Outcome::bad(why),
    };
    let root = match select_chain(root, &steps) {
        Ok(r) => r,
        Err(why) => return Outcome::fail(why, Value::Null),
    };
    let (got, ptr) = match resolve(root, eo.get("path")) {
        Ok(r) => r,
        Err(why) => return Outcome::bad(why),
    };
    let what = || describe(e);
    if op == "absent" {
        return match got {
            None => Outcome::ok(),
            Some(v) => Outcome::fail(format!("expected {}, got {}", what(), quote(v)), truncate(v.clone())),
        };
    }
    let Some(got) = got else { return Outcome::fail(format!("expected {}, but {} is not in the result", what(), label(&ptr)), Value::Null) };
    let length = eo.get("length").and_then(Value::as_bool).unwrap_or(false) || matches!(op, "len" | "lenGte" | "lenLte");
    let measured;
    let got = if length {
        match length_of(got) {
            Some(n) => {
                measured = json!(n);
                &measured
            }
            None => return Outcome::fail(format!("expected {}, but {} has no length (got {})", what(), label(&ptr), quote(got)), truncate(got.clone())),
        }
    } else {
        got
    };
    let want = eo.get("value");
    let need = |name: &str| -> Result<&Value, Outcome> { want.ok_or_else(|| Outcome::bad(format!("op `{name}` needs a `value`"))) };
    let num = |v: &Value| v.as_f64().filter(|x| x.is_finite());
    let result: Result<bool, Outcome> = (|| {
        Ok(match op {
            "exists" => true,
            "eq" => json_eq(got, need(op)?),
            "ne" => !json_eq(got, need(op)?),
            "approx" => {
                let tol = match eo.get("tol") {
                    None => DEFAULT_TOL,
                    Some(t) => num(t).filter(|t| *t >= 0.0).ok_or_else(|| Outcome::bad(format!("`tol` must be a number ≥ 0, got {}", quote(t))))?,
                };
                approx(got, need(op)?, tol).map_err(|why| Outcome::fail(format!("expected {}: {why}", what()), truncate(got.clone())))?
            }
            "gt" | "gte" | "lt" | "lte" | "len" | "lenGte" | "lenLte" => {
                let w = need(op)?;
                let (Some(a), Some(b)) = (num(got), num(w)) else {
                    return Err(Outcome::fail(format!("expected {}, but compares need numbers (got {})", what(), quote(got)), truncate(got.clone())));
                };
                match op {
                    "gt" => a > b,
                    "gte" | "lenGte" => a >= b,
                    "lt" => a < b,
                    "lte" | "lenLte" => a <= b,
                    _ => (a - b).abs() <= 1e-9,
                }
            }
            "contains" => match (got, need(op)?) {
                (Value::String(s), Value::String(w)) => s.to_lowercase().contains(&w.to_lowercase()),
                (Value::Array(a), w) => a.iter().any(|e| element_matches(e, w)),
                (Value::String(_), w) => return Err(Outcome::bad(format!("`contains` on a string needs a string value, got {}", quote(w)))),
                _ => {
                    return Err(Outcome::fail(
                        format!("expected {}, but `contains` works on strings and arrays (got {})", what(), quote(got)),
                        truncate(got.clone()),
                    ));
                }
            },
            "matches" => match (got, need(op)?) {
                (Value::String(s), Value::String(pat)) => glob(pat, s),
                (_, Value::String(_)) => {
                    return Err(Outcome::fail(format!("expected {}, but `matches` needs a string (got {})", what(), quote(got)), truncate(got.clone())));
                }
                (_, w) => return Err(Outcome::bad(format!("`matches` needs a pattern string (* and ?), got {}", quote(w)))),
            },
            "in" | "notIn" => {
                let Value::Array(set) = need(op)? else { return Err(Outcome::bad(format!("op `{op}` needs an array `value`"))) };
                set.iter().any(|w| json_eq(got, w)) == (op == "in")
            }
            _ => return Err(quantified(got, op, need(op)?, &ptr, depth)),
        })
    })();
    match result {
        Ok(true) => Outcome::ok(),
        Ok(false) => Outcome::fail(format!("expected {}, got {}", what(), quote(got)), truncate(got.clone())),
        Err(o) => o,
    }
}

/// `approx` on numbers, or element-wise on arrays of numbers (an expected array may be shorter:
/// `[960, 540]` matches the position `[960, 540, 0]`).
fn approx(got: &Value, want: &Value, tol: f64) -> Result<bool, String> {
    match (got, want) {
        (Value::Number(a), Value::Number(b)) => match (a.as_f64(), b.as_f64()) {
            (Some(a), Some(b)) if a.is_finite() && b.is_finite() => Ok((a - b).abs() <= tol),
            _ => Err("not a finite number".into()),
        },
        (Value::Array(a), Value::Array(b)) => {
            if b.len() > a.len() || b.is_empty() {
                return Err(format!("got {} values, expected {}", a.len(), b.len()));
            }
            for (x, y) in a.iter().zip(b) {
                if !approx(x, y, tol)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        _ => Err(format!("`approx` needs numbers or arrays of numbers (got {})", quote(got))),
    }
}

/// `any` / `all` / `none`: the elements of an array that meet every expectation of `value`
/// (paths relative to the element). Always returns an Outcome (pass or fail).
fn quantified(got: &Value, op: &str, want: &Value, ptr: &str, depth: usize) -> Outcome {
    if depth >= MAX_DEPTH {
        return Outcome::bad(format!("`any` / `all` / `none` nest at most {MAX_DEPTH} deep"));
    }
    let Value::Array(items) = got else { return Outcome::fail(format!("`{op}` needs an array at {}, got {}", label(ptr), quote(got)), truncate(got.clone())) };
    let tests: Vec<Value> = match want {
        Value::Array(a) => a.clone(),
        o @ Value::Object(_) => vec![o.clone()],
        w => return Outcome::bad(format!("`{op}` needs an expectation (or a list of them) as its value, got {}", quote(w))),
    };
    if tests.len() > MAX_EXPECT {
        return Outcome::bad(format!("too many element expectations ({}; at most {MAX_EXPECT})", tests.len()));
    }
    // A malformed element test fails the check (rather than silently matching nothing).
    let mut first_err = None;
    let mut meets = |e: &Value| -> bool {
        tests.iter().all(|t| {
            let o = expectation(e, t, depth + 1);
            if o.malformed && first_err.is_none() {
                first_err = Some(o.reason.clone());
            }
            o.pass
        })
    };
    let words = element_words(Some(want));
    let pass_ok = Outcome::ok();
    let out = match op {
        "any" => {
            if items.iter().any(&mut meets) {
                pass_ok
            } else {
                Outcome::fail(format!("expected an element of {} with {words}, found none among {} element(s)", label(ptr), items.len()), truncate(got.clone()))
            }
        }
        "all" => match items.iter().position(|e| !meets(e)) {
            None => pass_ok,
            Some(i) => {
                let bad = items.get(i).cloned().unwrap_or(Value::Null);
                Outcome::fail(format!("expected every element of {} to have {words}; element {i} doesn't: {}", label(ptr), quote(&bad)), bad)
            }
        },
        _ => {
            let hits: Vec<Value> = items.iter().filter(|e| meets(e)).cloned().collect();
            if hits.is_empty() {
                pass_ok
            } else {
                let first = hits.first().map(quote).unwrap_or_default();
                Outcome::fail(format!("expected no element of {} with {words}, found {}: {first}", label(ptr), hits.len()), truncate(Value::Array(hits)))
            }
        }
    };
    match first_err {
        Some(e) => Outcome::bad(e),
        None => out,
    }
}

/// Score of `passed` out of `total` (0 when there are no checks).
pub fn score(passed: usize, total: usize) -> f64 {
    if total == 0 {
        return 0.0;
    }
    let s = passed as f64 / total as f64;
    (s * 10000.0).round() / 10000.0
}
