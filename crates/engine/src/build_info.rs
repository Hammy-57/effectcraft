//! Build provenance and process/session identity shared by the app and its MCP executable.

#[cfg(not(target_family = "wasm"))]
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{Value, json};

#[cfg(not(target_family = "wasm"))]
static PROCESS_INSTANCE: OnceLock<String> = OnceLock::new();
static NEXT_SESSION: AtomicU64 = AtomicU64::new(1);

/// Stable for native processes. Wasm has no process nonce here: report it as unavailable.
#[cfg(not(target_family = "wasm"))]
pub fn process_instance() -> Option<&'static str> {
    Some(PROCESS_INSTANCE.get_or_init(|| {
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos();
        format!("{}-{stamp}-{:p}", std::process::id(), &PROCESS_INSTANCE)
    }))
}

/// Without host entropy, local wasm counters cannot identify distinct module instances.
#[cfg(target_family = "wasm")]
pub fn process_instance() -> Option<&'static str> {
    None
}

/// Stable for one Session lifetime; wasm IDs are explicitly local to their module instance.
pub fn next_session_id() -> String {
    format!("{}-{}", process_instance().unwrap_or("wasm-instance-local"), NEXT_SESSION.fetch_add(1, Ordering::Relaxed))
}

/// Metadata of the executable's engine code, captured when that code was compiled.
pub fn build() -> Value {
    let commit = env!("EFFECTCRAFT_BUILD_COMMIT");
    let dirty = match env!("EFFECTCRAFT_BUILD_DIRTY") {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    };
    json!({
        "commit": (commit != "unknown").then_some(commit),
        "dirty": dirty,
        "buildId": env!("EFFECTCRAFT_BUILD_ID"),
        "version": env!("CARGO_PKG_VERSION"),
        "target": env!("EFFECTCRAFT_BUILD_TARGET"),
        "profile": env!("EFFECTCRAFT_BUILD_PROFILE"),
    })
}

/// Features implemented by engine commands, including queries served by the live app.
pub fn engine_protocol_features() -> Value {
    json!({
        "inspectionReadIdentity": true,
        "confirmedWriteTimes": true,
        "ambiguousTargetsRejected": true,
        "atomicBatchAllowlist": true,
        "sessionIdentity": true,
        "processIdentity": cfg!(not(target_family = "wasm")),
        "projectOpenProvenance": true,
    })
}

/// The MCP executable adds bounded delivery, continuations, retry receipts and host routing.
pub fn protocol_features() -> Value {
    let mut features = engine_protocol_features();
    if let Some(fields) = features.as_object_mut() {
        for name in ["boundedInspectionReplies", "exactFieldChunks", "revisionGuardedPaging", "retryTombstones", "exactHostSelection", "scriptFailureSignals"] {
            fields.insert(name.into(), json!(true));
        }
    }
    features
}

/// Add the live session's identity without touching its document or contacting a host.
pub(crate) fn session_identity(s: &crate::Session, out: &mut Value) {
    if let Some(fields) = out.as_object_mut() {
        fields.insert("processInstance".into(), json!(process_instance()));
        fields.insert("sessionId".into(), json!(s.session_id));
        fields.insert("projectEpoch".into(), json!(s.epoch));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_metadata_has_explicit_provenance_and_protocol_features() {
        let metadata = build();
        assert!(metadata["buildId"].as_str().is_some_and(|id| !id.is_empty()));
        assert!(metadata["target"].as_str().is_some_and(|target| !target.is_empty()));
        assert!(metadata["dirty"].is_boolean() || metadata["dirty"].is_null());
        assert!(metadata["commit"].is_null() || metadata["commit"].as_str().is_some_and(|commit| matches!(commit.len(), 40 | 64)));
        assert_eq!(protocol_features()["sessionIdentity"], true);
        assert_eq!(build(), metadata, "build identity cannot drift at runtime");
    }

    #[test]
    fn process_identity_is_stable_and_sessions_are_distinct() {
        let a = crate::Session::default();
        let b = crate::Session::default();
        assert_eq!(process_instance(), process_instance());
        assert_ne!(a.session_id, b.session_id);
        if let Some(instance) = process_instance() {
            assert!(!instance.is_empty());
            assert!(a.session_id.starts_with(instance));
        } else {
            assert!(a.session_id.starts_with("wasm-instance-local-"));
        }
    }

    #[test]
    fn engine_capabilities_do_not_claim_mcp_delivery_features() {
        let engine = engine_protocol_features();
        let cli = protocol_features();
        for feature in
            ["exactFieldChunks", "boundedInspectionReplies", "revisionGuardedPaging", "retryTombstones", "exactHostSelection", "scriptFailureSignals"]
        {
            assert!(engine.get(feature).is_none());
            assert_eq!(cli[feature], true);
        }
        assert_eq!(engine["processIdentity"], cfg!(not(target_family = "wasm")));
    }

    #[cfg(target_family = "wasm")]
    #[test]
    fn wasm_process_identity_is_explicitly_unavailable() {
        assert_eq!(process_instance(), None);
        assert_eq!(serde_json::to_value(process_instance()).unwrap(), Value::Null);
    }

    #[test]
    fn project_replacement_changes_epoch_while_process_and_session_stay_stable() {
        let mut s = crate::Session::default();
        let before = s.execute("app.capabilities", json!({})).unwrap();
        s.replace_project(crate::project::Project::default(), None);
        let after = s.execute("app.capabilities", json!({})).unwrap();
        assert_eq!(after["processInstance"], before["processInstance"]);
        assert_eq!(after["sessionId"], before["sessionId"]);
        assert_eq!(after["projectEpoch"].as_u64(), before["projectEpoch"].as_u64().map(|epoch| epoch + 1));
        for command in ["editor.state", "project.summary"] {
            let state = s.execute(command, json!({})).unwrap();
            assert_eq!(state["processInstance"], after["processInstance"]);
            assert_eq!(state["sessionId"], after["sessionId"]);
            assert_eq!(state["projectEpoch"], after["projectEpoch"]);
        }
    }
}
