//! The host list and the save guard ([`crate::hosts`]).

use serde_json::json;

use crate::Session;

/// Regression for silent overwrites: two processes (or an agent's CLI) with one project file
/// open each saved over the other's work. A save over a file that changed on disk since this
/// process opened or saved it is refused, naming the ways out; `force` overwrites.
#[test]
fn saving_over_a_file_changed_elsewhere_is_refused() {
    let dir = std::env::temp_dir().join(format!("ec-save-guard-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("Shared.ecproj").to_string_lossy().into_owned();
    let mut a = Session::default();
    a.execute("comp.new", json!({"name": "Mine"})).unwrap();
    a.execute("file.saveAs", json!({"path": path})).unwrap();
    // Our own saves never trip the guard.
    a.execute("comp.new", json!({"name": "Mine too"})).unwrap();
    a.execute("file.save", json!({})).unwrap();
    // Someone else writes the file (another app, a CLI one-shot, a script).
    std::thread::sleep(std::time::Duration::from_millis(20));
    let theirs = std::fs::read_to_string(&path).unwrap().replace("Mine too", "Theirs changed");
    std::fs::write(&path, theirs + " ").unwrap();
    a.execute("comp.new", json!({"name": "Third"})).unwrap();
    let e = a.execute("file.save", json!({})).unwrap_err().to_string();
    assert!(e.contains("changed on disk") && e.contains("file.saveAs") && e.contains("force"), "{e}");
    assert!(std::fs::read_to_string(&path).unwrap().contains("Theirs changed"), "their work is untouched");
    // Reloading theirs, or forcing, clears it.
    a.execute("file.save", json!({"force": true})).unwrap();
    a.execute("comp.new", json!({"name": "Fourth"})).unwrap();
    a.execute("file.save", json!({})).unwrap();
    let _ = std::fs::remove_dir_all(dir);
}

/// Hosts announce themselves and the project they have open; `hosts.list` shows the live ones and
/// drops the files of processes that are gone; opening a project another host has open warns
/// with the port to join.
#[cfg(unix)]
#[test]
fn hosts_list_names_where_a_project_is_open() {
    let config = std::env::temp_dir().join(format!("ec-hosts-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&config);
    let hosts = config.join(crate::hosts::HOSTS_FOLDER);
    std::fs::create_dir_all(&hosts).unwrap();
    let project = config.join("Open.ecproj");
    let mut s = Session::default();
    s.execute("comp.new", json!({"name": "Main"})).unwrap();
    s.execute("file.saveAs", json!({"path": project.to_string_lossy()})).unwrap();
    let canonical = std::fs::canonicalize(&project).unwrap().to_string_lossy().into_owned();
    crate::hosts::reset();
    crate::hosts::announce(&config, "mcp", None);
    // Another live process (this test's parent, say) with the project open on port 9;
    // a dead one is dropped.
    let parent = std::os::unix::process::parent_id();
    std::fs::write(hosts.join(format!("{parent}.json")), json!({"pid": parent, "kind": "app", "port": 9, "project": canonical}).to_string()).unwrap();
    std::fs::write(hosts.join("999999.json"), json!({"pid": 999_999, "kind": "app", "port": 10, "project": canonical}).to_string()).unwrap();
    let r = s.execute("file.open", json!({"path": project.to_string_lossy()})).unwrap();
    assert_eq!(r["openElsewhere"][0]["port"], 9, "{r}");
    assert!(s.events.iter().any(|e| matches!(e, crate::Event::Toast { message, .. } if message.contains("--bridge 9"))));
    let list = s.execute("hosts.list", json!({})).unwrap()["hosts"].clone();
    let pids: Vec<u64> = list.as_array().unwrap().iter().filter_map(|h| h["pid"].as_u64()).collect();
    assert!(pids.contains(&u64::from(parent)) && pids.contains(&u64::from(std::process::id())), "{list}");
    assert!(!hosts.join("999999.json").exists(), "a gone process's file is removed");
    let me = list.as_array().unwrap().iter().find(|h| h["me"] == true).unwrap();
    assert_eq!((me["kind"].clone(), me["project"].clone()), (json!("mcp"), json!(canonical)));
    crate::hosts::withdraw();
    assert!(!hosts.join(format!("{}.json", std::process::id())).exists());
    crate::hosts::reset();
    let _ = std::fs::remove_dir_all(config);
}
