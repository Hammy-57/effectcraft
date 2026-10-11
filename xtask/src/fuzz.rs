//! `cargo xtask fuzz`: run the fuzz targets in `fuzz/` (gaps.md G3, AGENTS.md "Never crash").
//!
//! - `cargo xtask fuzz --list`: the targets.
//! - `cargo xtask fuzz --seeds`: (re)write the seed corpus `fuzz/corpus/<target>/seed-*`.
//! - `cargo xtask fuzz [TARGET…] [--seconds N] [--jobs N]`: fuzz each target (default: all) for N
//!   seconds (default 60) with cargo-fuzz on nightly. A crash stops it and leaves the input in
//!   `fuzz/artifacts/<target>/`; turn it into a regression test in the crate that crashed, then
//!   `cargo fuzz run <target> <artifact>` must pass.
//!
//! Needs `rustup toolchain install nightly` and `cargo install cargo-fuzz`. Inputs that hang are
//! failures too (`-timeout`, 10 s per input); memory is capped at 4 GB (`-rss_limit_mb`).

use std::path::Path;
use std::process::Command;

pub const TARGETS: &[&str] =
    &["project_open", "aep_ir", "lottie_import", "svg_parse", "psd_parse", "pdf_parse", "interchange_import", "expr_eval", "media_probe", "engine_command"];

pub fn run(root: &Path, args: &[String]) -> Result<(), String> {
    let dir = root.join("fuzz");
    let mut seconds = 60u64;
    let mut jobs = 1u32;
    let mut targets: Vec<String> = vec![];
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--list" => {
                println!("{}", TARGETS.join("\n"));
                return Ok(());
            }
            "--seeds" => return seeds(&dir),
            "--seconds" => seconds = it.next().and_then(|v| v.parse().ok()).filter(|s| *s > 0).ok_or("--seconds needs a whole number")?,
            "--jobs" => jobs = it.next().and_then(|v| v.parse().ok()).filter(|j| *j > 0).ok_or("--jobs needs a whole number")?,
            "-h" | "--help" => {
                println!("usage: cargo xtask fuzz [--list | --seeds | [TARGET…] [--seconds N] [--jobs N]]");
                return Ok(());
            }
            t if TARGETS.contains(&t) => targets.push(t.to_string()),
            other => return Err(format!("fuzz: unknown target or option {other} (targets: {})", TARGETS.join(", "))),
        }
    }
    if targets.is_empty() {
        targets = TARGETS.iter().map(|t| t.to_string()).collect();
    }
    let mut crashed = vec![];
    for t in &targets {
        println!("fuzz: {t} for {seconds} s");
        let corpus = dir.join("corpus").join(t);
        let _ = std::fs::create_dir_all(&corpus);
        let status = Command::new("cargo")
            .current_dir(&dir)
            .args(["+nightly", "fuzz", "run", t.as_str()])
            .arg(&corpus)
            .arg("--")
            .args(libfuzzer_args(t, seconds, jobs))
            .status()
            .map_err(|e| format!("fuzz: cannot run cargo fuzz (cargo install cargo-fuzz; rustup toolchain install nightly): {e}"))?;
        if !status.success() {
            crashed.push(t.clone());
        }
    }
    if crashed.is_empty() {
        println!("fuzz: no crashes in {}", targets.join(", "));
        Ok(())
    } else {
        Err(format!("fuzz: {} found a crash, hang or memory blow-up; the input is in fuzz/artifacts/<target>/", crashed.join(", ")))
    }
}

/// libFuzzer's options for one run of `target`.
fn libfuzzer_args(target: &str, seconds: u64, jobs: u32) -> Vec<String> {
    let mut a = vec![format!("-max_total_time={seconds}"), "-timeout=10".into(), "-rss_limit_mb=4096".into(), "-max_len=1048576".into()];
    // More than one job sends each one's output to fuzz-N.log; one job prints here.
    if jobs > 1 {
        a.push(format!("-jobs={jobs}"));
        a.push(format!("-workers={jobs}"));
    }
    // boa (the JavaScript engine) keeps some garbage-collected objects of a thrown exception
    // past the end of an evaluation, which LeakSanitizer reports; it is not EffectCraft's leak.
    if target == "expr_eval" {
        a.push("-detect_leaks=0".into());
    }
    a
}

fn seeds(dir: &Path) -> Result<(), String> {
    let status =
        Command::new("cargo").current_dir(dir).args(["+nightly", "run", "--example", "seeds"]).status().map_err(|e| format!("fuzz: cannot run cargo: {e}"))?;
    if status.success() { Ok(()) } else { Err("fuzz: writing the seeds failed".into()) }
}
