//! Control-channel replies that wait for background jobs, shared by the desktop app and the
//! windowless host (`effectcraft-cli serve`).
//!
//! A command that blocks until its job ends (a render, an import, a tracker; `wait: true`) would
//! hold up every other agent's request while it works. So the control channel runs it with
//! `wait: false`, keeps a [`JobWaiter`], serves other requests meanwhile, and sends the reply the
//! blocking command would have returned once the job ends. `jobs.wait` waits for the running tasks
//! the same way.

use serde_json::{Value, json};

use crate::Session;

/// A `wait: true` engine command (or `jobs.wait`) whose job runs in the background.
pub struct JobWaiter {
    pub command: String,
    pub params: Value,
    /// The command's own result (with `wait: false`).
    pub result: Value,
    /// Ids of the jobs it started (`jobs.list` ids: `render`, `track`, `roto`…).
    pub jobs: Vec<String>,
    /// Length of [`Session::offload_log`] when it started.
    pub log_len: usize,
}

/// What [`start`] did.
pub enum Started {
    /// The command ran (and returned this result) without a job to wait for.
    Done(Value),
    /// Reply once these jobs end ([`JobWaiter::finish`]).
    Waiting(JobWaiter),
}

/// Commands that wait for their job unless `wait: false` (others only with `wait: true`).
pub fn waits(command: &str, params: &Value) -> bool {
    // Frame renders and contact sheets too, so one agent's render never holds up the others.
    let default = matches!(command, "renderQueue.render" | "render.frame" | "render.contactSheet" | "file.importAep" | "file.importProject");
    params.get("wait").and_then(Value::as_bool).unwrap_or(default)
}

/// Ids of the background jobs `jobs.wait` waits for (the generic tasks).
pub fn task_ids(s: &Session) -> Vec<String> {
    s.jobs().into_iter().map(|j| j.id).filter(|j| j.starts_with("task:")).collect()
}

/// Run engine command `id` for the control channel. A waiting command runs with `wait: false`,
/// and when it started jobs the reply waits for them.
pub fn start(s: &mut Session, id: &str, params: Value) -> Result<Started, String> {
    if id == "jobs.wait" && !s.offloads() {
        let jobs = task_ids(s);
        if !jobs.is_empty() {
            let log_len = s.offload_log.len();
            return Ok(Started::Waiting(JobWaiter { command: id.to_string(), params, result: json!({}), jobs, log_len }));
        }
    }
    if !waits(id, &params) {
        return s.execute_checked(id, params).map(Started::Done).map_err(|e| e.to_string());
    }
    let before: Vec<String> = s.jobs().into_iter().map(|j| j.id).collect();
    let log_len = s.offload_log.len();
    let mut p = params.clone();
    if let Some(o) = p.as_object_mut() {
        o.insert("wait".into(), json!(false));
    }
    let result = s.execute_checked(id, p).map_err(|e| e.to_string())?;
    let jobs: Vec<String> = s.jobs().into_iter().map(|j| j.id).filter(|j| !before.contains(j)).collect();
    if jobs.is_empty() {
        return Ok(Started::Done(result));
    }
    Ok(Started::Waiting(JobWaiter { command: id.to_string(), params, result, jobs, log_len }))
}

impl JobWaiter {
    /// Whether its jobs are still running.
    pub fn pending(&self, s: &Session) -> bool {
        s.jobs().iter().any(|j| self.jobs.contains(&j.id))
    }

    /// The reply (`{ok, result}` / `{ok: false, error}`): what the blocking command would have
    /// returned (`running: false`, the final progress, render items and analysis status), or the
    /// job's error.
    pub fn finish(self, s: &mut Session) -> Value {
        s.poll_render();
        if self.command == "jobs.wait" {
            return json!({"ok": true, "result": {"finished": s.job_log}});
        }
        let errors: Vec<String> = s.offload_log.iter().skip(self.log_len).filter_map(|(_, e)| e.clone()).collect();
        if let Some(e) = errors.first() {
            return json!({"ok": false, "error": e});
        }
        // A generic task (desktop): the blocking command returned its result, or failed with its
        // message.
        let mut tasks: Vec<&mut crate::jobs::JobRecord> = s.job_log.iter_mut().filter(|r| self.jobs.contains(&r.id)).collect();
        if let Some(r) = tasks.iter().find(|r| r.status != "done") {
            return json!({"ok": false, "error": if r.message.is_empty() { r.status.clone() } else { r.message.clone() }});
        }
        if let [task] = tasks.as_mut_slice() {
            // Taken, not copied: a render's PNG shouldn't stay in the job log.
            return json!({"ok": true, "result": std::mem::take(&mut task.result)});
        }
        let mut r = self.result;
        if r.get("running").is_some() {
            r["running"] = json!(false);
        }
        if self.command == "renderQueue.render" {
            let ids: Vec<Value> = r["items"].as_array().map(|a| a.iter().map(|i| i["id"].clone()).collect()).unwrap_or_default();
            if let Ok(list) = s.execute("renderQueue.list", json!({})) {
                r["items"] =
                    json!(list["items"].as_array().map(|a| a.iter().filter(|i| ids.contains(&i["id"])).cloned().collect::<Vec<_>>()).unwrap_or_default());
            }
            r["rendering"] = json!(s.is_rendering());
        }
        let prefix = self.command.split('.').next().unwrap_or("");
        if matches!(prefix, "warp" | "camera" | "roto")
            && r.get("layer").is_some()
            && let Ok(st) = s.execute(&format!("{prefix}.status"), json!({"layer": r["layer"], "effect": r["effect"]}))
        {
            r["status"] = st;
        }
        if r.get("progress").is_some() {
            r["progress"] = Value::Null;
        }
        json!({"ok": true, "result": r})
    }
}
