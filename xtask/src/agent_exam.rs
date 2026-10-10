//! `cargo xtask agent-exam`: the agent exam (`evals/agent/`). About twenty motion-design tasks an
//! AI agent performs through the EffectCraft MCP server, scored by deterministic checks
//! (`exam.check`, run by the headless CLI).
//!
//! - `--list`: the tasks.
//! - `--validate`: parse every task file and check its shape (no CLI needed).
//! - `--check <project.ecproj> --task <task.json|id> [--json]`: score a finished project; exit 1
//!   when it fails.
//! - `--run [--tasks id,id] [--out DIR] [--minutes N] [--max-tool-calls N] [--model M]`: per task,
//!   build the starting project, run Claude Code headless against `effectcraft-cli mcp`, then
//!   score what it saved. Writes `report.json` and `report.md` (default
//!   `target/agent-exam/<timestamp>`). Meant for a nightly cloud job.
//!
//! The CLI is `$EFFECTCRAFT_CLI`, else `target/release/effectcraft-cli`, else
//! `cargo run -q --release -p effectcraft-cli --` (`--run` builds it once instead).

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// Check kinds (one per check; see evals/agent/README.md).
const KINDS: &[&str] = &["query", "lint", "motion", "compare", "prop", "layerCount", "comp"];
/// Expectation ops `exam.check` understands (keep in step with `crates/engine/src/exam.rs`).
const OPS: &[&str] = &[
    "eq", "ne", "approx", "gt", "gte", "lt", "lte", "contains", "matches", "exists", "absent", "len", "lenGte", "lenLte", "in", "notIn", "any", "all", "none",
];

const USAGE: &str = "usage: cargo xtask agent-exam --list | --validate
       | --check <project.ecproj> --task <task.json|id> [--json]
       | --run [--tasks id,id] [--out DIR] [--minutes N] [--max-tool-calls N] [--model M]
env: EFFECTCRAFT_CLI (CLI binary), AGENT_EXAM_CLAUDE (default `claude`),
     AGENT_EXAM_CLAUDE_ARGS (replaces the default flags; {mcp_config} {max_turns} {model} are filled in)";

pub fn run(root: &Path, args: &[String]) -> Result<(), String> {
    let opt = |k: &str| args.iter().position(|a| a == k).and_then(|i| args.get(i + 1)).cloned();
    let flag = |k: &str| args.iter().any(|a| a == k);
    let tasks_dir = root.join("evals/agent/tasks");
    if flag("--list") {
        for (_, t) in load_tasks(&tasks_dir)? {
            let n = t["checks"].as_array().map_or(0, Vec::len);
            println!("{:<28} {} ({n} checks)", t["id"].as_str().unwrap_or("?"), t["title"].as_str().unwrap_or(""));
        }
        return Ok(());
    }
    if flag("--validate") {
        return validate_all(&tasks_dir);
    }
    if flag("--check") {
        let project = opt("--check").ok_or(USAGE)?;
        let task = opt("--task").ok_or("--check needs --task <task.json|id>")?;
        let (path, task) = find_task(&tasks_dir, &task)?;
        let cli = Cli::find(root, false)?;
        let r = cli.check(Path::new(&project), &task, &base_of(&path))?;
        if flag("--json") {
            println!("{r}");
        } else {
            print_result(&task, &r);
        }
        return if r["passed"] == json!(true) { Ok(()) } else { Err(format!("{}: failed (score {})", task["id"].as_str().unwrap_or("?"), r["score"])) };
    }
    if flag("--run") {
        let cfg = RunConfig {
            only: opt("--tasks").map(|s| s.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect()),
            out: opt("--out").map(PathBuf::from).unwrap_or_else(|| target_dir(root).join("agent-exam").join(timestamp())),
            minutes: opt("--minutes").map(|m| m.parse::<f64>().map_err(|_| format!("--minutes: not a number: {m}"))).transpose()?,
            max_tool_calls: opt("--max-tool-calls").map(|m| m.parse::<u64>().map_err(|_| format!("--max-tool-calls: not a number: {m}"))).transpose()?,
            model: opt("--model"),
        };
        return run_exam(root, &tasks_dir, &cfg);
    }
    Err(USAGE.into())
}

// ---------------------------------------------------------------- tasks

fn load_tasks(dir: &Path) -> Result<Vec<(PathBuf, Value)>, String> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    files.sort();
    files.into_iter().map(|p| read_task(&p).map(|t| (p, t))).collect()
}

fn read_task(p: &Path) -> Result<Value, String> {
    let text = std::fs::read_to_string(p).map_err(|e| format!("{}: {e}", p.display()))?;
    serde_json::from_str(&text).map_err(|e| format!("{}: not valid JSON: {e}", p.display()))
}

/// A task by file path or id.
fn find_task(dir: &Path, which: &str) -> Result<(PathBuf, Value), String> {
    let p = Path::new(which);
    if p.is_file() {
        return Ok((p.to_path_buf(), read_task(p)?));
    }
    let tasks = load_tasks(dir)?;
    let ids: Vec<String> = tasks.iter().filter_map(|(_, t)| t["id"].as_str().map(str::to_string)).collect();
    tasks.into_iter().find(|(_, t)| t["id"] == json!(which)).ok_or_else(|| format!("no task `{which}` (tasks: {})", ids.join(", ")))
}

/// The folder a task's relative paths (reference images, projects) resolve against.
fn base_of(task_file: &Path) -> PathBuf {
    let dir = task_file.parent().unwrap_or(Path::new("."));
    std::path::absolute(dir).unwrap_or_else(|_| dir.to_path_buf())
}

fn validate_all(dir: &Path) -> Result<(), String> {
    let tasks = load_tasks(dir)?;
    let mut problems = vec![];
    let mut seen = std::collections::HashSet::new();
    let mut checks = 0;
    for (p, t) in &tasks {
        let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        for e in validate(t) {
            problems.push(format!("{name}: {e}"));
        }
        if let Some(id) = t["id"].as_str() {
            if !seen.insert(id.to_string()) {
                problems.push(format!("{name}: duplicate task id `{id}`"));
            }
            if p.file_stem().is_some_and(|s| s != id) {
                problems.push(format!("{name}: the file name should be `{id}.json`"));
            }
        }
        checks += t["checks"].as_array().map_or(0, Vec::len);
    }
    if problems.is_empty() {
        println!("agent-exam: {} tasks, {checks} checks: ok", tasks.len());
        Ok(())
    } else {
        Err(format!("agent-exam: {} problem(s):\n  {}", problems.len(), problems.join("\n  ")))
    }
}

/// Shape problems of one task (empty when it is fine). Mirrors what `exam.check` accepts.
pub fn validate(t: &Value) -> Vec<String> {
    let mut e = vec![];
    let Some(o) = t.as_object() else { return vec!["a task must be a JSON object".into()] };
    for k in ["id", "title", "prompt"] {
        if o.get(k).and_then(Value::as_str).is_none_or(|s| s.trim().is_empty()) {
            e.push(format!("`{k}` must be a non-empty string"));
        }
    }
    if let Some(id) = o.get("id").and_then(Value::as_str)
        && !id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        e.push(format!("id `{id}`: use lowercase letters, digits and dashes"));
    }
    match (o.get("setup").filter(|v| !v.is_null()), o.get("project").filter(|v| !v.is_null())) {
        (Some(_), Some(_)) => e.push("give `setup` or `project`, not both".into()),
        (Some(s), None) => steps(s, "setup", &mut e),
        (None, Some(p)) if !p.is_string() => e.push("`project` must be a relative .ecproj path".into()),
        _ => {}
    }
    if let Some(s) = o.get("solution").filter(|v| !v.is_null()) {
        steps(s, "solution", &mut e);
    }
    match o.get("limits") {
        Some(Value::Object(l)) => {
            if l.get("minutes").and_then(Value::as_f64).is_none_or(|m| m <= 0.0) {
                e.push("`limits.minutes` must be a number > 0".into());
            }
            if l.get("toolCalls").and_then(Value::as_u64).is_none_or(|n| n == 0) {
                e.push("`limits.toolCalls` must be a whole number > 0".into());
            }
        }
        _ => e.push("`limits` must be {minutes, toolCalls}".into()),
    }
    match o.get("checks").and_then(Value::as_array) {
        Some(cs) if !cs.is_empty() => {
            let mut ids = std::collections::HashSet::new();
            for (i, c) in cs.iter().enumerate() {
                let id = c.get("id").and_then(Value::as_str).map_or_else(|| format!("check {}", i + 1), str::to_string);
                if c.get("id").and_then(Value::as_str).is_none() {
                    e.push(format!("{id}: needs an `id`"));
                } else if !ids.insert(id.clone()) {
                    e.push(format!("{id}: duplicate check id"));
                }
                for p in check_problems(c) {
                    e.push(format!("{id}: {p}"));
                }
            }
        }
        _ => e.push("`checks` must be a non-empty array".into()),
    }
    e
}

fn steps(s: &Value, what: &str, e: &mut Vec<String>) {
    let Some(a) = s.as_array() else {
        e.push(format!("`{what}` must be an array of {{command, params}}"));
        return;
    };
    for (i, st) in a.iter().enumerate() {
        if st.get("command").and_then(Value::as_str).is_none_or(str::is_empty) {
            e.push(format!("{what} step {}: needs `command`", i + 1));
        }
        if st.get("params").is_some_and(|p| !p.is_object()) {
            e.push(format!("{what} step {}: `params` must be an object", i + 1));
        }
    }
}

fn check_problems(c: &Value) -> Vec<String> {
    let mut e = vec![];
    let Some(o) = c.as_object() else { return vec!["a check must be an object".into()] };
    let kinds: Vec<&str> = KINDS.iter().copied().filter(|k| o.contains_key(*k)).collect();
    let [kind] = kinds.as_slice() else {
        return vec![format!("needs exactly one of {} (has {})", KINDS.join(", "), if kinds.is_empty() { "none".into() } else { kinds.join(", ") })];
    };
    let op_ok = |e: &mut Vec<String>| match o.get("op").and_then(Value::as_str) {
        Some(op) if OPS.contains(&op) => {}
        Some(op) => e.push(format!("unknown op `{op}`")),
        None => e.push("needs `op`".into()),
    };
    match *kind {
        "query" => match o.get("expect").and_then(Value::as_array) {
            Some(x) if !x.is_empty() => x.iter().for_each(|x| expectation_problems(x, &mut e, 0)),
            _ => e.push("needs a non-empty `expect` array".into()),
        },
        "prop" => {
            if o["prop"].get("layer").is_none() || o["prop"].get("path").and_then(Value::as_str).is_none() {
                e.push("`prop` needs `layer` and `path`".into());
            }
            op_ok(&mut e);
        }
        "layerCount" => op_ok(&mut e),
        "comp" => {
            if o["comp"].get("name").is_none() {
                e.push("`comp` needs `name`".into());
            }
        }
        "motion" => {
            if o.get("layer").is_none() {
                e.push("needs `layer`".into());
            }
            let segs = match o.get("segment") {
                Some(s @ Value::Object(_)) => vec![s.clone()],
                Some(Value::Array(a)) => a.clone(),
                _ => vec![],
            };
            if segs.is_empty() || segs.iter().any(|s| s.get("kind").and_then(Value::as_str).is_none()) {
                e.push("needs `segment` with a `kind`".into());
            }
        }
        "compare" => {
            if o["compare"].get("reference").and_then(Value::as_str).is_none() {
                e.push("`compare` needs `reference`".into());
            }
        }
        "lint" => {
            if let Some(m) = o.get("maxSeverity").and_then(Value::as_str)
                && !["info", "warning", "none"].contains(&m)
            {
                e.push(format!("maxSeverity `{m}`: info, warning or none"));
            }
        }
        _ => {}
    }
    e
}

fn expectation_problems(x: &Value, e: &mut Vec<String>, depth: usize) {
    match x.get("op").and_then(Value::as_str) {
        Some(op) if OPS.contains(&op) => {
            if !["exists", "absent"].contains(&op) && x.get("value").is_none() {
                e.push(format!("op `{op}` needs a `value`"));
            }
            if ["any", "all", "none"].contains(&op) && depth < 6 {
                match x.get("value") {
                    Some(Value::Array(a)) => a.iter().for_each(|v| expectation_problems(v, e, depth + 1)),
                    Some(v @ Value::Object(_)) => expectation_problems(v, e, depth + 1),
                    _ => e.push(format!("`{op}` needs an expectation (or a list of them) as its value")),
                }
            }
        }
        Some(op) => e.push(format!("unknown op `{op}`")),
        None => e.push("an expectation needs `op`".into()),
    }
}

fn print_result(task: &Value, r: &Value) {
    println!(
        "{}: {} — {}/{} checks, score {}",
        task["id"].as_str().unwrap_or("?"),
        if r["passed"] == json!(true) { "PASS" } else { "FAIL" },
        r["passedCount"],
        r["total"],
        r["score"]
    );
    for c in r["results"].as_array().into_iter().flatten() {
        let mark = if c["pass"] == json!(true) { "ok  " } else { "FAIL" };
        let why = c["reason"].as_str().filter(|s| !s.is_empty()).map(|s| format!(" — {s}")).unwrap_or_default();
        println!("  {mark} {} {}{why}", c["id"].as_str().unwrap_or("?"), c["desc"].as_str().unwrap_or(""));
    }
}

// ---------------------------------------------------------------- the CLI

/// How to run `effectcraft-cli`: a program and the arguments before the subcommand.
struct Cli {
    program: String,
    pre: Vec<String>,
}

fn target_dir(root: &Path) -> PathBuf {
    std::env::var_os("CARGO_TARGET_DIR").map(PathBuf::from).unwrap_or_else(|| root.join("target"))
}

impl Cli {
    /// `build`: build the release CLI when there is none (for `--run`, whose MCP config needs a
    /// binary); otherwise fall back to `cargo run`.
    fn find(root: &Path, build: bool) -> Result<Cli, String> {
        if let Some(p) = std::env::var_os("EFFECTCRAFT_CLI") {
            return Ok(Cli { program: p.to_string_lossy().into_owned(), pre: vec![] });
        }
        let bin = target_dir(root).join("release").join(if cfg!(windows) { "effectcraft-cli.exe" } else { "effectcraft-cli" });
        if !bin.is_file() && build {
            eprintln!("agent-exam: building the release CLI (cargo build --release -p effectcraft-cli)…");
            let st = Command::new(env!("CARGO"))
                .args(["build", "--release", "-p", "effectcraft-cli", "--manifest-path"])
                .arg(root.join("Cargo.toml"))
                .status()
                .map_err(|e| e.to_string())?;
            if !st.success() {
                return Err("building effectcraft-cli failed".into());
            }
        }
        if bin.is_file() {
            return Ok(Cli { program: bin.to_string_lossy().into_owned(), pre: vec![] });
        }
        eprintln!("agent-exam: no release CLI; running it with `cargo run --release` (builds it first)");
        let manifest = root.join("Cargo.toml").to_string_lossy().into_owned();
        Ok(Cli {
            program: env!("CARGO").into(),
            pre: ["run", "-q", "--release", "--manifest-path", &manifest, "-p", "effectcraft-cli", "--"].map(String::from).to_vec(),
        })
    }

    fn command(&self) -> Command {
        let mut c = Command::new(&self.program);
        c.args(&self.pre);
        c
    }

    /// Run the CLI with `args`; its last stdout line parsed as JSON.
    fn json(&self, args: &[String]) -> Result<Value, String> {
        let out = self.command().args(args).stderr(Stdio::inherit()).output().map_err(|e| format!("{}: {e}", self.program))?;
        let stdout = String::from_utf8_lossy(&out.stdout);
        let last = stdout.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("");
        let v: Value = serde_json::from_str(last)
            .map_err(|_| format!("effectcraft-cli {}: no JSON output (exit {})", args.first().map_or("", String::as_str), out.status))?;
        if let Some(e) = v.get("error") {
            return Err(format!("effectcraft-cli: {e}"));
        }
        Ok(v)
    }

    /// `exam.check` on a saved project.
    fn check(&self, project: &Path, task: &Value, base: &Path) -> Result<Value, String> {
        let params = json!({"task": task, "base": base.to_string_lossy()});
        self.json(&[
            "exec".into(),
            "exam.check".into(),
            "--project".into(),
            project.to_string_lossy().into_owned(),
            "--params".into(),
            params.to_string(),
            "--json".into(),
        ])
    }

    /// Build a task's starting project at `out` (from `setup`, or a copy of `project`).
    fn start_project(&self, task: &Value, base: &Path, out: &Path) -> Result<(), String> {
        if let Some(p) = task.get("project").and_then(Value::as_str) {
            return std::fs::copy(base.join(p), out).map(|_| ()).map_err(|e| format!("copy {p}: {e}"));
        }
        let mut args: Vec<String> = vec!["run".into()];
        for st in task.get("setup").and_then(Value::as_array).into_iter().flatten() {
            args.push(st["command"].as_str().unwrap_or_default().to_string());
            args.push(st.get("params").cloned().unwrap_or_else(|| json!({})).to_string());
        }
        if args.len() == 1 {
            // An empty project: one harmless query so `run` has something to do.
            args.extend(["project.summary".into(), "{}".into()]);
        }
        args.extend(["--empty".into(), "--save-as".into(), out.to_string_lossy().into_owned(), "--json".into()]);
        self.json(&args).map(|_| ())
    }
}

// ---------------------------------------------------------------- --run

struct RunConfig {
    only: Option<Vec<String>>,
    out: PathBuf,
    minutes: Option<f64>,
    max_tool_calls: Option<u64>,
    model: Option<String>,
}

/// One task's row in the report.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Row {
    pub id: String,
    pub title: String,
    pub pass: bool,
    pub score: f64,
    pub minutes: f64,
    pub tool_calls: u64,
    /// (check id, reason) of each failed check.
    pub failures: Vec<(String, String)>,
    pub error: Option<String>,
}

impl Row {
    fn to_json(&self) -> Value {
        json!({
            "id": self.id, "title": self.title, "pass": self.pass, "score": self.score,
            "minutes": (self.minutes * 100.0).round() / 100.0, "toolCalls": self.tool_calls,
            "failures": self.failures.iter().map(|(id, why)| json!({"id": id, "reason": why})).collect::<Vec<_>>(),
            "error": self.error,
        })
    }
}

fn run_exam(root: &Path, dir: &Path, cfg: &RunConfig) -> Result<(), String> {
    let mut tasks = load_tasks(dir)?;
    if let Some(only) = &cfg.only {
        for id in only {
            if !tasks.iter().any(|(_, t)| t["id"] == json!(id)) {
                return Err(format!("no task `{id}`"));
            }
        }
        tasks.retain(|(_, t)| t["id"].as_str().is_some_and(|id| only.iter().any(|o| o == id)));
    }
    let bad: Vec<String> = tasks.iter().flat_map(|(_, t)| validate(t).into_iter().map(|e| format!("{}: {e}", t["id"]))).collect();
    if !bad.is_empty() {
        return Err(format!("invalid tasks (cargo xtask agent-exam --validate):\n  {}", bad.join("\n  ")));
    }
    let cli = Cli::find(root, true)?;
    std::fs::create_dir_all(&cfg.out).map_err(|e| format!("{}: {e}", cfg.out.display()))?;
    let out = std::path::absolute(&cfg.out).unwrap_or_else(|_| cfg.out.clone());
    let mut rows = vec![];
    for (path, task) in &tasks {
        let id = task["id"].as_str().unwrap_or("task").to_string();
        eprintln!("agent-exam: {id}…");
        let row = run_task(&cli, path, task, &out.join(&id), cfg).unwrap_or_else(|e| Row {
            id: id.clone(),
            title: task["title"].as_str().unwrap_or("").into(),
            error: Some(e),
            ..Row::default()
        });
        eprintln!(
            "agent-exam: {id}: {} (score {}, {:.1} min, {} tool calls){}",
            if row.pass { "PASS" } else { "FAIL" },
            row.score,
            row.minutes,
            row.tool_calls,
            row.error.as_ref().map(|e| format!(" — {e}")).unwrap_or_default()
        );
        rows.push(row);
        // Keep the report current, so an interrupted nightly run still leaves one.
        write_reports(&out, &rows, cfg)?;
    }
    let passed = rows.iter().filter(|r| r.pass).count();
    println!("agent-exam: {passed}/{} tasks passed; report: {}", rows.len(), out.join("report.md").display());
    Ok(())
}

fn write_reports(out: &Path, rows: &[Row], cfg: &RunConfig) -> Result<(), String> {
    let json = json!({
        "model": cfg.model,
        "summary": summary(rows),
        "tasks": rows.iter().map(Row::to_json).collect::<Vec<_>>(),
    });
    let pretty = serde_json::to_string_pretty(&json).map_err(|e| e.to_string())?;
    std::fs::write(out.join("report.json"), pretty).map_err(|e| e.to_string())?;
    std::fs::write(out.join("report.md"), report_markdown(rows, cfg.model.as_deref())).map_err(|e| e.to_string())
}

fn run_task(cli: &Cli, path: &Path, task: &Value, dir: &Path, cfg: &RunConfig) -> Result<Row, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let base = base_of(path);
    let start = dir.join("start.ecproj");
    let work = dir.join("work.ecproj");
    cli.start_project(task, &base, &start)?;
    std::fs::copy(&start, &work).map_err(|e| format!("copy start project: {e}"))?;
    let mut server_args = cli.pre.clone();
    server_args.extend(["mcp".into(), "--project".into(), work.to_string_lossy().into_owned()]);
    let mcp = json!({"mcpServers": {"effectcraft": {"command": cli.program, "args": server_args}}});
    let mcp_path = dir.join("mcp.json");
    std::fs::write(&mcp_path, mcp.to_string()).map_err(|e| e.to_string())?;

    let limits = &task["limits"];
    let minutes = cfg.minutes.or_else(|| limits["minutes"].as_f64()).unwrap_or(10.0).max(0.1);
    let max_calls = cfg.max_tool_calls.or_else(|| limits["toolCalls"].as_u64()).unwrap_or(60).max(1);
    let prompt = agent_prompt(task, &work, minutes, max_calls);
    std::fs::write(dir.join("prompt.txt"), &prompt).map_err(|e| e.to_string())?;

    let claude = std::env::var("AGENT_EXAM_CLAUDE").unwrap_or_else(|_| "claude".into());
    // Turns: one per tool call plus the final answer.
    let args = claude_args(&mcp_path, max_calls.saturating_add(2), cfg.model.as_deref(), std::env::var("AGENT_EXAM_CLAUDE_ARGS").ok().as_deref());
    let stream_path = dir.join("stream.jsonl");
    let stream = std::fs::File::create(&stream_path).map_err(|e| e.to_string())?;
    let errlog = std::fs::File::create(dir.join("claude.stderr")).map_err(|e| e.to_string())?;
    let t0 = Instant::now();
    let mut child = Command::new(&claude)
        .arg("-p")
        .arg(&prompt)
        .args(&args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(stream)
        .stderr(errlog)
        .spawn()
        .map_err(|e| format!("cannot start `{claude}`: {e} (install Claude Code or set AGENT_EXAM_CLAUDE)"))?;
    let deadline = Duration::from_secs_f64(minutes * 60.0);
    let mut error = None;
    loop {
        match child.try_wait() {
            Ok(Some(st)) => {
                if !st.success() {
                    error = Some(format!("claude exited with {st}"));
                }
                break;
            }
            Ok(None) if t0.elapsed() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                error = Some(format!("time limit: stopped after {minutes} min"));
                break;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(500)),
            Err(e) => {
                error = Some(e.to_string());
                break;
            }
        }
    }
    let elapsed = t0.elapsed().as_secs_f64() / 60.0;
    let tool_calls = count_tool_calls(&std::fs::read_to_string(&stream_path).unwrap_or_default());
    let mut row = Row {
        id: task["id"].as_str().unwrap_or("").into(),
        title: task["title"].as_str().unwrap_or("").into(),
        minutes: elapsed,
        tool_calls,
        error,
        ..Row::default()
    };
    if tool_calls > max_calls {
        row.failures.push(("limits".into(), format!("used {tool_calls} tool calls, the limit is {max_calls}")));
    }
    match cli.check(&work, task, &base) {
        Ok(r) => {
            std::fs::write(dir.join("check.json"), r.to_string()).map_err(|e| e.to_string())?;
            row.score = r["score"].as_f64().unwrap_or(0.0);
            row.pass = r["passed"] == json!(true) && tool_calls <= max_calls;
            for c in r["results"].as_array().into_iter().flatten().filter(|c| c["pass"] != json!(true)) {
                row.failures.push((c["id"].as_str().unwrap_or("?").into(), c["reason"].as_str().unwrap_or("").into()));
            }
        }
        Err(e) => row.error = Some(row.error.map_or(e.clone(), |x| format!("{x}; {e}"))),
    }
    Ok(row)
}

/// What the agent is told: the task, then how to work and save.
pub fn agent_prompt(task: &Value, work: &Path, minutes: f64, max_calls: u64) -> String {
    format!(
        "{}\n\n---\nYou are working in EffectCraft, an After Effects-class motion graphics app, through its MCP server \
         (the mcp__effectcraft__* tools; command_list and describe_command show every engine command). The project \
         is already open. Use the exact comp and layer names given above. When you are done, save the project with \
         save_project (path: {}) — unsaved work is not graded. Limits: {minutes} minutes, {max_calls} tool calls. \
         Don't ask questions; finish the task on your own.",
        task["prompt"].as_str().unwrap_or("").trim(),
        work.display()
    )
}

/// Claude Code's flags after `-p <prompt>`: the defaults, or `override_args` (whitespace-split,
/// with `{mcp_config}`, `{max_turns}` and `{model}` filled in).
pub fn claude_args(mcp: &Path, max_turns: u64, model: Option<&str>, override_args: Option<&str>) -> Vec<String> {
    let mcp = mcp.to_string_lossy().into_owned();
    if let Some(o) = override_args.filter(|o| !o.trim().is_empty()) {
        return o
            .split_whitespace()
            .map(|a| a.replace("{mcp_config}", &mcp).replace("{max_turns}", &max_turns.to_string()).replace("{model}", model.unwrap_or("")))
            .collect();
    }
    let mut v: Vec<String> = vec![
        "--mcp-config".into(),
        mcp,
        "--strict-mcp-config".into(),
        "--output-format".into(),
        "stream-json".into(),
        "--verbose".into(),
        "--max-turns".into(),
        max_turns.to_string(),
        "--allowedTools".into(),
        "mcp__effectcraft__*".into(),
    ];
    if let Some(m) = model {
        v.extend(["--model".into(), m.into()]);
    }
    v
}

/// Tool calls in a Claude Code `stream-json` transcript: `tool_use` blocks of assistant messages.
pub fn count_tool_calls(stream: &str) -> u64 {
    let mut n = 0u64;
    for line in stream.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        if v["type"] != json!("assistant") {
            continue;
        }
        let blocks = v["message"]["content"].as_array().map_or(0, |c| c.iter().filter(|b| b["type"] == json!("tool_use")).count());
        n = n.saturating_add(blocks as u64);
    }
    n
}

/// Pass rate, mean score, mean minutes and tool calls.
pub fn summary(rows: &[Row]) -> Value {
    let n = rows.len();
    let mean = |f: &dyn Fn(&Row) -> f64| if n == 0 { 0.0 } else { rows.iter().map(f).sum::<f64>() / n as f64 };
    let r2 = |x: f64| (x * 100.0).round() / 100.0;
    let passed = rows.iter().filter(|r| r.pass).count();
    json!({
        "tasks": n,
        "passed": passed,
        "passRate": if n == 0 { 0.0 } else { r2(passed as f64 / n as f64) },
        "meanScore": r2(mean(&|r| r.score)),
        "meanMinutes": r2(mean(&|r| r.minutes)),
        "meanToolCalls": r2(mean(&|r| r.tool_calls as f64)),
    })
}

/// The Markdown report: summary, one table row per task, then the failures with their reasons.
pub fn report_markdown(rows: &[Row], model: Option<&str>) -> String {
    let s = summary(rows);
    let mut md = String::from("# Agent exam\n\n");
    if let Some(m) = model {
        md.push_str(&format!("Model: `{m}`\n\n"));
    }
    md.push_str(&format!(
        "**{}/{} tasks passed** ({:.0} %), mean score {}, mean {} min, mean {} tool calls.\n\n",
        s["passed"],
        s["tasks"],
        s["passRate"].as_f64().unwrap_or(0.0) * 100.0,
        s["meanScore"],
        s["meanMinutes"],
        s["meanToolCalls"]
    ));
    md.push_str("| Task | Result | Score | Minutes | Tool calls |\n|---|---|---|---|---|\n");
    for r in rows {
        let result = if r.pass {
            "pass"
        } else if r.error.is_some() {
            "error"
        } else {
            "fail"
        };
        md.push_str(&format!("| `{}` {} | {result} | {:.2} | {:.1} | {} |\n", r.id, r.title.replace('|', "/"), r.score, r.minutes, r.tool_calls));
    }
    let failing: Vec<&Row> = rows.iter().filter(|r| !r.pass).collect();
    if !failing.is_empty() {
        md.push_str("\n## Failures\n");
        for r in failing {
            md.push_str(&format!("\n### `{}`\n\n", r.id));
            if let Some(e) = &r.error {
                md.push_str(&format!("- error: {e}\n"));
            }
            for (id, why) in &r.failures {
                md.push_str(&format!("- `{id}`: {why}\n"));
            }
        }
    }
    md
}

/// `YYYYMMDD-HHMMSS` (UTC) for the default output folder.
fn timestamp() -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let (days, rem) = ((secs / 86_400) as i64, secs % 86_400);
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}{m:02}{d:02}-{:02}{:02}{:02}", rem / 3600, rem % 3600 / 60, rem % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_tool_use_blocks_of_assistant_messages() {
        let stream = [
            r#"{"type":"system","subtype":"init","tools":["mcp__effectcraft__command_run"]}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Let me look."},{"type":"tool_use","id":"a","name":"mcp__effectcraft__get_comp","input":{}}]}}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"a","content":"{}"}]}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"b","name":"x","input":{}},{"type":"tool_use","id":"c","name":"y","input":{}}]}}"#,
            "not json",
            r#"{"type":"result","subtype":"success","num_turns":3}"#,
        ]
        .join("\n");
        assert_eq!(count_tool_calls(&stream), 3);
        assert_eq!(count_tool_calls(""), 0);
    }

    #[test]
    fn report_summarises_and_lists_failures() {
        let rows = vec![
            Row { id: "a".into(), title: "A".into(), pass: true, score: 1.0, minutes: 2.0, tool_calls: 10, ..Row::default() },
            Row {
                id: "b".into(),
                title: "B | b".into(),
                pass: false,
                score: 0.5,
                minutes: 4.0,
                tool_calls: 20,
                failures: vec![("c1".into(), "expected 1, got 2".into())],
                error: None,
            },
        ];
        let s = summary(&rows);
        assert_eq!(s["passed"], 1);
        assert_eq!(s["passRate"], 0.5);
        assert_eq!(s["meanScore"], 0.75);
        assert_eq!(s["meanMinutes"], 3.0);
        assert_eq!(s["meanToolCalls"], 15.0);
        let md = report_markdown(&rows, Some("m"));
        assert!(md.contains("**1/2 tasks passed** (50 %)"), "{md}");
        assert!(md.contains("| `b` B / b | fail | 0.50 | 4.0 | 20 |"), "{md}");
        assert!(md.contains("- `c1`: expected 1, got 2"), "{md}");
        assert!(!md.contains("### `a`"), "{md}");
        assert_eq!(summary(&[])["passRate"], 0.0);
    }

    #[test]
    fn claude_args_default_and_override() {
        let a = claude_args(Path::new("/x/mcp.json"), 42, Some("opus"), None);
        assert!(a.windows(2).any(|w| w == ["--max-turns", "42"]));
        assert!(a.windows(2).any(|w| w == ["--model", "opus"]));
        assert!(a.contains(&"--strict-mcp-config".to_string()));
        let o = claude_args(Path::new("/x/mcp.json"), 7, None, Some("--mcp-config {mcp_config} --max-turns {max_turns}"));
        assert_eq!(o, ["--mcp-config", "/x/mcp.json", "--max-turns", "7"]);
    }

    #[test]
    fn validate_flags_bad_shapes() {
        let good = json!({"id": "t", "title": "T", "prompt": "p", "setup": [{"command": "comp.new", "params": {}}], "limits": {"minutes": 5, "toolCalls": 30},
            "checks": [{"id": "c", "prop": {"layer": "L", "path": "transform/opacity"}, "op": "eq", "value": 0}]});
        assert!(validate(&good).is_empty(), "{:?}", validate(&good));
        let bad = json!({"id": "T x", "title": "", "prompt": "p", "limits": {"minutes": 0}, "checks": [{"id": "c", "query": "comp.info", "expect": [{"op": "nope"}]}, {"prop": {}, "lint": {}}]});
        let e = validate(&bad);
        for want in ["`title`", "id `T x`", "limits.minutes", "limits.toolCalls", "unknown op `nope`", "needs an `id`", "exactly one of"] {
            assert!(e.iter().any(|x| x.contains(want)), "missing {want}: {e:?}");
        }
    }

    #[test]
    fn timestamp_has_the_right_shape() {
        let t = timestamp();
        assert_eq!(t.len(), 15, "{t}");
        assert_eq!(t.as_bytes().get(8), Some(&b'-'));
    }
}
