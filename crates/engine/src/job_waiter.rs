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
    known_jobs: std::sync::Mutex<Vec<String>>,
    pub command: String,
    pub params: Value,
    /// The command's own result (with `wait: false`).
    pub result: Value,
    /// Ids of the jobs it started (`jobs.list` ids: `render`, `track`, `roto`…).
    pub jobs: Vec<String>,
    /// Absolute completion-log position when it started (including evicted entries).
    pub log_cursor: usize,
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
            let log_cursor = s.offload_log_start.saturating_add(s.offload_log.len());
            return Ok(Started::Waiting(JobWaiter {
                known_jobs: std::sync::Mutex::new(jobs.clone()),
                command: id.to_string(),
                params,
                result: json!({}),
                jobs,
                log_cursor,
            }));
        }
    }
    if !waits(id, &params) {
        return s.execute_checked(id, params).map(Started::Done).map_err(|e| e.to_string());
    }
    let before: Vec<String> = s.jobs().into_iter().map(|j| j.id).collect();
    let log_cursor = s.offload_log_start.saturating_add(s.offload_log.len());
    let mut p = params.clone();
    if let Some(o) = p.as_object_mut() {
        o.insert("wait".into(), json!(false));
    }
    let result = s.execute_checked(id, p).map_err(|e| e.to_string())?;
    let jobs: Vec<String> = s.jobs().into_iter().map(|j| j.id).filter(|j| !before.contains(j)).collect();
    if jobs.is_empty() {
        return Ok(Started::Done(result));
    }
    Ok(Started::Waiting(JobWaiter { known_jobs: std::sync::Mutex::new(jobs.clone()), command: id.to_string(), params, result, jobs, log_cursor }))
}

impl JobWaiter {
    /// A waiter for `jobs` started by `command`, created when the completion log was at `log_cursor`.
    pub fn new(command: &str, params: Value, result: Value, jobs: Vec<String>, log_cursor: usize) -> Self {
        Self { known_jobs: std::sync::Mutex::new(jobs.clone()), command: command.to_string(), params, result, jobs, log_cursor }
    }

    /// A staged task can publish `nextJob`; latch that child before the old log entry ages
    /// out so `wait: true` means full completion, not merely a draft becoming available.
    fn follow_jobs(&self, s: &Session) -> Vec<String> {
        let mut known = self.known_jobs.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut index = 0usize;
        while index < known.len() && known.len() < self.jobs.len().saturating_add(64) {
            let next = known
                .get(index)
                .and_then(|id| s.job_log.iter().find(|j| &j.id == id))
                .and_then(|j| j.result.get("nextJob"))
                .and_then(Value::as_str)
                .map(str::to_owned);
            if let Some(next) = next
                && next.starts_with("task:")
                && !known.contains(&next)
            {
                known.push(next);
            }
            index += 1;
        }
        known.clone()
    }
    /// Whether its jobs are still running.
    pub fn pending(&self, s: &Session) -> bool {
        let known = self.follow_jobs(s);
        s.jobs().iter().any(|j| known.contains(&j.id))
    }

    /// The reply (`{ok, result}` / `{ok: false, error}`): what the blocking command would have
    /// returned (`running: false`, the final progress, render items and analysis status), or the
    /// job's error.
    pub fn finish(self, s: &mut Session) -> Value {
        s.poll_render();
        let known = self.follow_jobs(s);
        if self.command == "jobs.wait" {
            return json!({"ok": true, "result": {"finished": s.job_log}});
        }
        // Only this command's own jobs count: another analysis may fail in the same frame.
        let error = s
            .offload_log
            .iter()
            .skip(self.log_cursor.saturating_sub(s.offload_log_start))
            .filter(|(kind, _)| self.jobs.iter().any(|id| id == kind.job_id()))
            .find_map(|(_, e)| e.as_ref());
        if let Some(e) = error {
            return json!({"ok": false, "error": e});
        }
        // A generic task (desktop): the blocking command returned its result, or failed with its
        // message.
        let mut tasks: Vec<&mut crate::jobs::JobRecord> = s.job_log.iter_mut().filter(|r| known.contains(&r.id)).collect();
        if let Some(r) = tasks.iter().find(|r| r.status != "done") {
            return json!({"ok": false, "error": if r.message.is_empty() { r.status.clone() } else { r.message.clone() }});
        }
        if self.jobs.len() == 1
            && let Some(task) = tasks.iter_mut().find(|t| Some(&t.id) == known.last())
        {
            // Taken, not copied: a render's PNG shouldn't stay in the job log.
            return json!({"ok": true, "result": std::mem::take(&mut task.result)});
        }
        if self.command.starts_with("freeze.") && !known.is_empty() && tasks.is_empty() {
            return json!({"ok":false,"error":"Freeze job result expired; inspect freeze.list for the current state"});
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
