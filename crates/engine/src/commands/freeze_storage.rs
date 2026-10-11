//! Smart Freeze storage controls. Measurements and retirement run off the UI thread.

use super::{CommandSpec, b_p, bad};
use crate::{Result, Session, cmd};
use serde_json::{Value, json};

fn storage(s: &mut Session, p: &Value, cleanup: bool) -> Result<Value> {
    if s.project.freeze.namespace.is_empty() {
        return Ok(json!({"bytes":0,"versions":[]}));
    }
    if s.tasks.iter().any(|t| matches!(t.kind, "smartFreeze" | "freezeStorage")) {
        return Err(bad("freeze.storage", "wait for the current freeze/storage job to finish"));
    }
    let project = s.project.clone();
    let immediate = b_p(p, "retireNow").unwrap_or(false);
    s.spawn_task("freezeStorage", "Smart Freeze storage", b_p(p, "wait").unwrap_or(false), move |ctl| {
        let root = crate::freeze_storage::root(&project.freeze.namespace)?;
        let report = if cleanup {
            crate::freeze_storage::maintain(
                &root,
                &crate::freeze_storage::protected(&project),
                crate::freeze_runtime::now_millis() / 1000,
                immediate,
                project.freeze.budget_bytes,
                project.freeze.retire_old,
                |path| {
                    if ctl.cancelled() {
                        return Err("Smart Freeze cancelled".into());
                    }
                    crate::freeze_storage::move_to_trash(path)
                },
            )?
        } else {
            crate::freeze_storage::Report { bytes: crate::freeze_storage::usage(&root)?, ..Default::default() }
        };
        let history = crate::freeze_storage::history(&root)?;
        let total = history.len();
        let history: Vec<_> = history.into_iter().rev().take(32).collect();
        Ok(Box::new(move |_| Ok(json!({"namespace":project.freeze.namespace,"storage":report,"versions":history,"totalVersions":total}))))
    })
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!("freeze.storage", "Measure Freeze Storage", [], None, "{wait?}", super::always, |s, p| storage(s, p, false)),
        cmd!("freeze.cleanup", "Clean Freeze Storage", [], None, "{retireNow?, wait?}", super::always, |s, p| storage(s, p, true)),
        cmd!("freeze.settings", "Freeze Storage Settings", [], None, "{budgetGiB?, retireOld?}", super::always, |s, p| {
            let budget = match p.get("budgetGiB") {
                Some(v) => Some(
                    v.as_f64()
                        .filter(|v| v.is_finite() && *v >= 0.0 && *v <= 8192.0)
                        .ok_or_else(|| bad("freeze.settings", "budgetGiB must be between 0 and 8192"))?,
                ),
                None => None,
            };
            let retirement = match p.get("retireOld") {
                Some(v) => Some(v.as_bool().ok_or_else(|| bad("freeze.settings", "retireOld must be a boolean"))?),
                None => None,
            };
            s.edit("Smart Freeze Storage Settings", None, |project, _| {
                if let Some(v) = budget {
                    project.freeze.budget_bytes = (v * 1024.0 * 1024.0 * 1024.0) as u64;
                }
                if let Some(v) = retirement {
                    project.freeze.retire_old = v;
                }
                Ok(())
            })?;
            Ok(json!({"budgetBytes":s.project.freeze.budget_bytes,"retireOld":s.project.freeze.retire_old}))
        }),
        cmd!("freeze.restore", "Restore Retired Freeze", [], None, "{directory, wait?}", super::always, |s, p| {
            let directory = super::str_p(p, "directory").ok_or_else(|| bad("freeze.restore", "directory is required"))?.to_owned();
            let namespace = s.project.freeze.namespace.clone();
            s.spawn_task("freezeStorage", "Restore retired Smart Freeze", b_p(p, "wait").unwrap_or(false), move |_| {
                crate::freeze_storage::restore(&crate::freeze_storage::root(&namespace)?, &directory)?;
                Ok(Box::new(move |s| {
                    s.freeze_runtime.bad_versions.remove(&directory);
                    s.freeze_runtime.generation = s.freeze_runtime.generation.saturating_add(1);
                    if s.project.freeze.namespace == namespace {
                        let _ = s.execute("freeze.storage", json!({}));
                    }
                    Ok(json!({"restored":directory}))
                }))
            })
        }),
    ]
}
