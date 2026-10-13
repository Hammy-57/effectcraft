//! Every task in `evals/agent/tasks` solved by its reference solution in the fully wired session,
//! expressions included (the engine's own `every_task_is_solvable` has no expression engine, so it
//! leaves out the checks that read `expressionError`).

use serde_json::{Value, json};

#[test]
fn every_task_is_solvable_with_expressions() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../evals/agent/tasks");
    let mut files: Vec<_> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().path()).filter(|p| p.extension().is_some_and(|x| x == "json")).collect();
    files.sort();
    assert!(files.len() >= 15, "{} tasks", files.len());
    for f in files {
        let task: Value = serde_json::from_str(&std::fs::read_to_string(&f).unwrap()).unwrap_or_else(|e| panic!("{}: {e}", f.display()));
        let id = task["id"].as_str().unwrap().to_string();
        let checks = task["checks"].clone();
        let mut s = effectcraft_host::session();
        for st in task["setup"].as_array().into_iter().flatten() {
            s.execute_checked(st["command"].as_str().unwrap(), st.get("params").cloned().unwrap_or(json!({})))
                .unwrap_or_else(|e| panic!("{id} setup {st}: {e}"));
        }
        let base = f.parent().unwrap().to_string_lossy().into_owned();
        for st in task["solution"].as_array().unwrap_or_else(|| panic!("{id}: no solution")) {
            s.execute_checked(st["command"].as_str().unwrap(), st.get("params").cloned().unwrap_or(json!({})))
                .unwrap_or_else(|e| panic!("{id} solution {st}: {e}"));
        }
        let r = s.execute("exam.check", json!({"checks": checks, "base": base})).unwrap();
        let failed: Vec<String> = r["results"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|x| x["pass"] != true)
            .map(|x| format!("{}: {} (actual {})", x["id"], x["reason"], x["actual"]))
            .collect();
        assert!(failed.is_empty(), "{id}: the solution fails\n  {}", failed.join("\n  "));
    }
}
