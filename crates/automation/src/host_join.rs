//! Explicit, unique host selection. Discovery entries are hints; confirmation uses live data.

use std::path::Path;

use serde_json::{Map, Value, json};

use crate::{Error, Result};

fn entries(hosts: &Value) -> Result<&[Value]> {
    hosts
        .as_array()
        .or_else(|| hosts.get("hosts").and_then(Value::as_array))
        .map(Vec::as_slice)
        .ok_or_else(|| Error::BadArgs("host discovery returned no `hosts` array".into()))
}

fn valid_port(value: &Value) -> Option<u16> {
    value.as_u64().and_then(|p| u16::try_from(p).ok()).filter(|p| *p != 0)
}

fn candidate(host: &Value) -> Value {
    let mut out = Map::new();
    for key in ["port", "pid", "kind", "project", "processInstance", "sessionId", "projectEpoch"] {
        if let Some(value) = host.get(key) {
            out.insert(key.into(), value.clone());
        }
    }
    Value::Object(out)
}

fn candidates(hosts: &[&Value]) -> Value {
    Value::Array(hosts.iter().map(|host| candidate(host)).collect())
}

/// `hosts` is hosts.list's object or its raw array. A numeric port is explicit even when
/// discovery is stale; path joins require one canonical match, including non-joinable hosts.
pub(crate) fn select_port(hosts: &Value, join: &Value) -> Result<u16> {
    if join.is_number() {
        return valid_port(join).ok_or_else(|| Error::BadArgs(format!("`join`: {join} is not a port (1–65535)")));
    }
    let project =
        join.as_str().filter(|p| !p.is_empty()).ok_or_else(|| Error::BadArgs(format!("`join` is a numeric port or a full project path, not {join}")))?;
    if project.len() > 65_536 {
        return Err(Error::BadArgs("`join` project path exceeds 64 KiB".into()));
    }
    let all = entries(hosts)?;
    let path = Path::new(project);
    let name = path.file_name();
    let by_name: Vec<_> = all
        .iter()
        .filter(|host| name.is_some_and(|name| host.get("project").and_then(Value::as_str).and_then(|p| Path::new(p).file_name()) == Some(name)))
        .collect();
    if !path.is_absolute() && path.parent().is_none_or(|parent| parent.as_os_str().is_empty()) {
        return Err(Error::BadArgs(format!(
            "basename-only project {project:?} cannot select a host; candidates: {}; join with an exact full project path or a numeric port",
            candidates(&by_name)
        )));
    }
    let wanted = std::fs::canonicalize(path).map_err(|error| {
        Error::BadArgs(format!(
            "cannot canonicalize project path {project:?}: {error}; candidates with the same basename: {}; use a numeric port or an existing full project path",
            candidates(&by_name)
        ))
    })?;
    let matching: Vec<_> = all
        .iter()
        .filter(|host| host.get("project").and_then(Value::as_str).and_then(|project| std::fs::canonicalize(project).ok()).is_some_and(|path| path == wanted))
        .collect();
    let host = match matching.as_slice() {
        [] => {
            return Err(Error::BadArgs(format!(
                "no running EffectCraft has exact project path {} open; candidates with the same basename: {}; open_project or explicitly join a numeric port",
                wanted.display(),
                candidates(&by_name)
            )));
        }
        [host] => *host,
        _ => {
            return Err(Error::BadArgs(format!(
                "project {} is open in multiple host sessions; candidates: {}; choose a numeric port explicitly",
                wanted.display(),
                candidates(&matching)
            )));
        }
    };
    host.get("port")
        .and_then(valid_port)
        .ok_or_else(|| Error::BadArgs(format!("project {} has one matching host but no usable control port; host: {}", wanted.display(), candidate(host))))
}

/// Confirm path selection against the connected host before replacing the current backend.
/// Explicit numeric ports intentionally join whichever project that host currently holds.
pub(crate) fn verify_connected_path(join: &Value, state: &Value) -> Result<()> {
    if valid_port(join).is_some() {
        return Ok(());
    }
    let requested = join
        .as_str()
        .filter(|path| !path.is_empty() && path.len() <= 65_536)
        .ok_or_else(|| Error::BadArgs("`join` must be a valid numeric port or project path".into()))?;
    let actual = state.get("projectPath").and_then(Value::as_str)
        .filter(|path| path.len() <= 65_536 && Path::new(path).is_absolute())
        .ok_or_else(|| Error::BadArgs(format!(
            "connected host did not report an absolute current project path (reported {}, session {}); the backend was not switched; select a numeric port explicitly or update the host",
            state.get("projectPath").unwrap_or(&Value::Null), state.get("sessionId").unwrap_or(&Value::Null)
        )))?;
    let wanted = std::fs::canonicalize(requested)
        .map_err(|error| Error::BadArgs(format!("cannot verify requested project path {requested:?}: {error}; the backend was not switched")))?;
    let live = std::fs::canonicalize(actual)
        .map_err(|error| Error::BadArgs(format!("cannot verify connected host project path {actual:?}: {error}; the backend was not switched")))?;
    if wanted != live {
        return Err(Error::BadArgs(format!(
            "connected host has project {} open instead of requested {} (session {}); discovery changed before connection; the backend was not switched",
            live.display(),
            wanted.display(),
            state.get("sessionId").unwrap_or(&Value::Null)
        )));
    }
    Ok(())
}

/// Short identity from the connected backend's host.info and project.summary, never from
/// discovery. In particular a host that switched projects confirms its actual current path.
pub(crate) fn identity(port: Option<u16>, actual_host: &Value, summary: &Value, client: Option<&str>) -> Value {
    let mut out = Map::new();
    if let Some(port) = port {
        out.insert("port".into(), json!(port));
    }
    for key in ["pid", "kind", "processInstance", "sessionId", "projectEpoch", "build"] {
        if let Some(value) = summary.get(key).or_else(|| actual_host.get(key)) {
            out.insert(key.into(), value.clone());
        }
    }
    if let Some(path) = summary.get("projectPath").or_else(|| summary.get("path")) {
        match path.as_str() {
            Some(reported) if !Path::new(reported).is_absolute() => {
                out.insert("path".into(), Value::Null);
                out.insert("reportedPath".into(), path.clone());
                out.insert("pathResolved".into(), json!(false));
            }
            Some(reported) => {
                let resolved = std::fs::canonicalize(reported).map(|path| json!(path.to_string_lossy())).unwrap_or_else(|_| path.clone());
                out.insert("path".into(), resolved);
            }
            None => {
                out.insert("path".into(), path.clone());
            }
        }
    }
    for key in ["revision", "dirty"] {
        if let Some(value) = summary.get(key) {
            out.insert(key.into(), value.clone());
        }
    }
    if let Some(client) = client {
        out.insert("client".into(), json!(client));
    }
    Value::Object(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
            let dir = std::env::temp_dir().join(format!("effectcraft-host-join-{}-{stamp}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        fn project(&self, folder: &str) -> PathBuf {
            let dir = self.0.join(folder);
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join("Same.ecproj");
            std::fs::write(&path, "{}").unwrap();
            path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn host(port: Value, pid: u64, path: &Path) -> Value {
        json!({"port": port, "pid": pid, "kind": "app", "project": path.to_string_lossy()})
    }

    #[test]
    fn same_basename_never_selects_a_host_even_when_unique() {
        let temp = TempDir::new();
        let a = temp.project("one");
        let b = temp.project("two");
        for hosts in [json!([host(json!(9877), 1, &a)]), json!([host(json!(9877), 1, &a), host(json!(9878), 2, &b)])] {
            let err = select_port(&hosts, &json!("Same.ecproj")).unwrap_err().to_string();
            assert!(err.contains("basename-only") && err.contains("9877") && err.contains("one"), "{err}");
        }
    }

    #[test]
    fn exact_path_distinguishes_projects_with_same_basename() {
        let temp = TempDir::new();
        let a = temp.project("one");
        let b = temp.project("two");
        let hosts = json!({"hosts": [host(json!(9877), 1, &a), host(json!(9878), 2, &b)]});
        assert_eq!(select_port(&hosts, &json!(b.to_string_lossy())).unwrap(), 9878);
        assert_eq!(select_port(&hosts, &json!(a.parent().unwrap().join("../one/Same.ecproj").to_string_lossy())).unwrap(), 9877);
        let alias = a.parent().unwrap().join("../one/Same.ecproj");
        let summary = json!({"path": alias.to_string_lossy()});
        assert_eq!(
            identity(None, &json!({"kind": "bridge"}), &summary, None),
            json!({"kind": "bridge", "path": std::fs::canonicalize(&a).unwrap().to_string_lossy()})
        );
    }

    #[test]
    fn exact_path_open_in_multiple_sessions_requires_explicit_port() {
        let temp = TempDir::new();
        let a = temp.project("one");
        let hosts = json!([host(json!(9877), 1, &a), host(json!(9878), 2, &a)]);
        let err = select_port(&hosts, &json!(a.to_string_lossy())).unwrap_err().to_string();
        assert!(err.contains("multiple host sessions") && err.contains("9877") && err.contains("9878"), "{err}");
        assert_eq!(select_port(&hosts, &json!(9878)).unwrap(), 9878);
    }

    #[test]
    fn nonjoinable_duplicate_still_counts_as_another_session() {
        let temp = TempDir::new();
        let a = temp.project("one");
        let hosts = json!([host(json!(9877), 1, &a), host(Value::Null, 2, &a)]);
        assert!(select_port(&hosts, &json!(a.to_string_lossy())).unwrap_err().to_string().contains("multiple host sessions"));
        let hosts = json!([host(Value::Null, 2, &a)]);
        assert!(select_port(&hosts, &json!(a.to_string_lossy())).unwrap_err().to_string().contains("no usable control port"));
    }

    #[test]
    fn full_path_mismatch_does_not_fall_back_to_basename() {
        let temp = TempDir::new();
        let a = temp.project("one");
        let b = temp.project("two");
        let hosts = json!([host(json!(9877), 1, &a)]);
        let err = select_port(&hosts, &json!(b.to_string_lossy())).unwrap_err().to_string();
        assert!(err.contains("no running EffectCraft has exact") && err.contains("9877"), "{err}");
    }

    #[test]
    fn missing_paths_offer_candidates_without_selecting_them() {
        let temp = TempDir::new();
        let a = temp.project("one");
        let hosts = json!([host(json!(9877), 1, &a)]);
        let absent = temp.0.join("absent/Same.ecproj");
        let err = select_port(&hosts, &json!(absent.to_string_lossy())).unwrap_err().to_string();
        assert!(err.contains("cannot canonicalize") && err.contains("9877"), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn symlink_paths_match_and_duplicate_aliases_remain_ambiguous() {
        let temp = TempDir::new();
        let a = temp.project("one");
        let alias = temp.0.join("linked.ecproj");
        std::os::unix::fs::symlink(&a, &alias).unwrap();
        assert_eq!(select_port(&json!([host(json!(9877), 1, &a)]), &json!(alias.to_string_lossy())).unwrap(), 9877);
        assert_eq!(
            identity(None, &Value::Null, &json!({"path": alias.to_string_lossy()}), None)["path"],
            json!(std::fs::canonicalize(&a).unwrap().to_string_lossy())
        );
        let hosts = json!([host(json!(9877), 1, &a), host(json!(9878), 2, &alias)]);
        assert!(select_port(&hosts, &json!(a.to_string_lossy())).unwrap_err().to_string().contains("multiple host sessions"));
    }

    #[test]
    fn explicit_numeric_ports_work_with_stale_discovery_but_malformed_ports_fail() {
        assert_eq!(select_port(&json!([]), &json!(9877)).unwrap(), 9877);
        assert_eq!(select_port(&Value::Null, &json!(65535)).unwrap(), 65535);
        for port in [json!(0), json!(65536), json!(-1), json!(9877.5), Value::Null, json!(true), json!("9877")] {
            assert!(select_port(&json!([]), &port).is_err(), "{port}");
        }
    }

    #[test]
    fn malformed_discovery_and_join_arguments_are_errors() {
        assert!(select_port(&json!({}), &json!("/any/project.ecproj")).is_err());
        assert!(select_port(&json!([]), &json!("")).is_err());
        assert!(select_port(&json!([]), &json!("x".repeat(65_537))).is_err());
    }

    #[test]
    fn identity_uses_the_connected_summary_path_and_session_data() {
        let actual = json!({"pid": 42, "project": "/old/discovery.ecproj", "sessionId": "host-session", "projectEpoch": 2});
        let summary = json!({"path": "/actual/current.ecproj", "revision": 7, "dirty": true, "processInstance": "live", "sessionId": "host-session", "projectEpoch": 3, "build": {"commit": "tested"}, "items": [{"large": "omitted"}]});
        let result = identity(Some(9877), &actual, &summary, Some("agent"));
        assert_eq!(
            result,
            json!({"port": 9877, "pid": 42, "processInstance": "live", "sessionId": "host-session", "projectEpoch": 3, "build": {"commit": "tested"}, "path": "/actual/current.ecproj", "revision": 7, "dirty": true, "client": "agent"})
        );
        assert_eq!(identity(None, &Value::Null, &json!({"path": null}), None), json!({"path": null}));
    }

    #[test]
    fn live_path_verification_rejects_stale_discovery_but_explicit_port_joins() {
        let temp = TempDir::new();
        let advertised = temp.project("advertised");
        let current = temp.project("current");
        let state = json!({"projectPath": current.to_string_lossy(), "sessionId": "live-session"});
        let err = verify_connected_path(&json!(advertised.to_string_lossy()), &state).unwrap_err().to_string();
        assert!(err.contains("instead of requested") && err.contains("backend was not switched") && err.contains("live-session"), "{err}");
        verify_connected_path(&json!(9877), &state).unwrap();
        verify_connected_path(&json!(9877), &json!({"projectPath": null})).unwrap();
    }

    #[test]
    fn live_path_verification_requires_an_absolute_reported_path() {
        let temp = TempDir::new();
        let requested = temp.project("one");
        for state in [json!({}), json!({"projectPath": null}), json!({"projectPath": "Same.ecproj"}), json!({"projectPath": 42})] {
            assert!(verify_connected_path(&json!(requested.to_string_lossy()), &state).unwrap_err().to_string().contains("absolute current project path"));
        }
        verify_connected_path(&json!(requested.to_string_lossy()), &json!({"projectPath": requested.to_string_lossy()})).unwrap();
        for join in [json!(0), json!(65536), json!(true), json!("")] {
            assert!(verify_connected_path(&join, &json!({})).is_err());
        }
    }

    #[cfg(unix)]
    #[test]
    fn live_symlink_path_verification_matches_the_same_file() {
        let temp = TempDir::new();
        let project = temp.project("one");
        let alias = temp.0.join("linked.ecproj");
        std::os::unix::fs::symlink(&project, &alias).unwrap();
        verify_connected_path(&json!(alias.to_string_lossy()), &json!({"projectPath": project.to_string_lossy()})).unwrap();
        verify_connected_path(&json!(project.to_string_lossy()), &json!({"projectPath": alias.to_string_lossy()})).unwrap();
    }

    #[test]
    fn legacy_relative_identity_paths_are_explicitly_unresolved() {
        // Cargo.toml exists in the CLI test cwd, but tells us nothing about the app's cwd.
        assert_eq!(
            identity(None, &Value::Null, &json!({"path": "Cargo.toml"}), None),
            json!({"path": null, "reportedPath": "Cargo.toml", "pathResolved": false})
        );
        assert_eq!(
            identity(None, &Value::Null, &json!({"projectPath": "Same.ecproj", "path": "/ignored/other.ecproj"}), None),
            json!({"path": null, "reportedPath": "Same.ecproj", "pathResolved": false})
        );
    }
}
