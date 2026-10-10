//! `exam.check`: score the current project against an agent-exam task's checks (`evals/agent/`,
//! `cargo xtask agent-exam`). Each check runs one read-only query command and tests its result
//! ([`crate::exam`]). Only queries run: commands that edit the project (journaled), write files or
//! run other commands are refused, so a task file can't change what it grades.

use serde_json::{Value, json};

use super::{CommandSpec, bad, str_p};
use crate::{Result, Session, exam, query};

/// Not journaled, but not read-only either: they run other commands, write files (`render.frame`
/// and `render.contactSheet` always write a PNG), import, store a token, or (this command) recurse.
const DENY: &[&str] = &[
    "exam.check",
    "render.frame",
    "render.contactSheet",
    "engine.batch",
    "journal.export",
    "help.exportRepro",
    "file.importFigma",
    "figma.setToken",
    "test.panic",
];

/// Parameters of otherwise read-only queries that write a file (refused).
const DENY_PARAMS: &[(&str, &str)] = &[("render.compare", "diff_path"), ("audio.analyze", "path"), ("mediaBrowser.list", "path")];

/// Why `query` may not run as a check (`None`: it may).
pub(crate) fn refused(query: &str, params: &Value) -> Option<String> {
    let Some(spec) = super::find(query) else { return Some(format!("unknown command `{query}` (command.list lists them)")) };
    if spec.journal {
        return Some(format!("`{query}` changes the project; checks may only run read-only query commands"));
    }
    if DENY.contains(&query) {
        return Some(format!("`{query}` is not a read-only query; checks may not run it"));
    }
    if let Some((_, k)) = DENY_PARAMS.iter().find(|(id, k)| *id == query && params.get(*k).is_some_and(|v| !v.is_null())) {
        return Some(format!("`{query}` with `{k}` writes a file; checks may not write files"));
    }
    None
}

fn result(id: String, desc: String, pass: bool, reason: String, actual: Value) -> Value {
    json!({"id": id, "desc": desc, "pass": pass, "reason": reason, "actual": actual})
}

/// Run one check (index `i`) against the session.
fn run_check(s: &mut Session, c: &Value, i: usize, base: Option<&str>) -> Value {
    let (id, desc) = (exam::check_id(c, i), exam::check_desc(c));
    let chk = match exam::expand(c, i, base) {
        Ok(chk) => chk,
        Err(why) => return result(id, desc, false, format!("malformed check: {why}"), Value::Null),
    };
    if let Some(why) = refused(&chk.query, &chk.params) {
        return result(id, desc, false, why, Value::Null);
    }
    match s.execute_checked(&chk.query, chk.params.clone()) {
        Ok(v) => {
            let o = exam::evaluate(&chk, &v);
            let reason = if o.malformed { format!("malformed check: {}", o.reason) } else { o.reason };
            result(id, desc, o.pass, reason, o.actual)
        }
        Err(e) => result(id, desc, false, format!("{} failed: {e}", chk.query), Value::Null),
    }
}

/// `exam.check {task | checks, base?}`
fn check(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "exam.check";
    let checks = match (p.get("task").filter(|t| !t.is_null()), p.get("checks").filter(|c| !c.is_null())) {
        (Some(_), Some(_)) => return Err(bad(C, "give `task` or `checks`, not both")),
        (Some(t), None) => {
            let t = t.as_object().ok_or_else(|| bad(C, "`task` must be a task object (evals/agent/README.md)"))?;
            t.get("checks").and_then(Value::as_array).ok_or_else(|| bad(C, "the task has no `checks` array"))?
        }
        (None, Some(c)) => c.as_array().ok_or_else(|| bad(C, "`checks` must be an array of checks"))?,
        (None, None) => return Err(bad(C, "missing `task` (a task object) or `checks` (an array of checks)")),
    };
    if checks.len() > exam::MAX_CHECKS {
        return Err(bad(C, format!("{} checks; at most {} run at once", checks.len(), exam::MAX_CHECKS)));
    }
    let base = str_p(p, "base");
    let checks = checks.clone();
    let results: Vec<Value> = checks.iter().enumerate().map(|(i, c)| run_check(s, c, i, base)).collect();
    let total = results.len();
    let passed = results.iter().filter(|r| r["pass"] == json!(true)).count();
    let mut out = json!({
        "passed": total > 0 && passed == total,
        "score": exam::score(passed, total),
        "total": total,
        "passedCount": passed,
        "results": results,
    });
    if let Some(id) = p.get("task").and_then(|t| t.get("id")).filter(|v| !v.is_null()) {
        out["task"] = id.clone();
    }
    Ok(out)
}

pub fn specs() -> Vec<CommandSpec> {
    vec![query!(
        "exam.check",
        "Check Agent Exam Task",
        "{task?: {id, checks: [...]} | checks?: [check], base? (folder relative reference paths resolve against)} → {passed, score (0..1), total, passedCount, results: [{id, desc, pass, reason, actual}]}; a check runs one read-only query: {query, params, select?, expect: [{path (JSON Pointer), op, value, tol?}]} or a shorthand (lint, motion, compare, prop, layerCount, comp); see evals/agent/README.md",
        check
    )]
}
