//! `cargo xtask repros [--bless] [--dir DIR]…`: replay every repro script (`tests/repros/`, plus
//! the gitignored `plan/repros/` when it exists, plus each `--dir`) and check its expectations and
//! `golden` frames. The scripts run in `effectcraft-host`'s `repros` integration test, so this is
//! the same check the test suite makes, with the private corpora added.
//!
//! `--bless` writes the `golden` frames instead of comparing them: run it once after a change
//! that is meant to alter a render, look at the new PNGs, and commit them.

use std::path::Path;
use std::process::Command;

pub fn run(root: &Path, args: &[String]) -> Result<(), String> {
    let mut bless = false;
    let mut dirs = vec![];
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--bless" => bless = true,
            "--dir" => dirs.push(root.join(it.next().ok_or("--dir needs a folder")?)),
            "-h" | "--help" => {
                println!("usage: cargo xtask repros [--bless] [--dir DIR]…  (tests/repros, plan/repros and each DIR)");
                return Ok(());
            }
            other => return Err(format!("repros: unknown argument {other} (try --help)")),
        }
    }
    let private = root.join("plan/repros");
    if private.is_dir() {
        dirs.push(private);
    }
    for d in &dirs {
        if !d.is_dir() {
            return Err(format!("repros: {} is not a folder", d.display()));
        }
    }
    let mut cmd = Command::new(env!("CARGO"));
    cmd.current_dir(root).args(["test", "-p", "effectcraft-host", "--test", "host", "repros::", "--", "--nocapture"]);
    if !dirs.is_empty() {
        let joined = std::env::join_paths(&dirs).map_err(|e| format!("repros: {e}"))?;
        cmd.env("EFFECTCRAFT_REPROS", joined);
    }
    if bless {
        cmd.env("EFFECTCRAFT_BLESS", "1");
    }
    let names: Vec<String> = dirs.iter().map(|d| d.display().to_string()).collect();
    println!(
        "repros: tests/repros{}{}",
        if names.is_empty() { String::new() } else { format!(" + {}", names.join(", ")) },
        if bless { " (blessing goldens)" } else { "" }
    );
    let status = cmd.status().map_err(|e| format!("repros: cannot run cargo: {e}"))?;
    if status.success() { Ok(()) } else { Err("repros: a repro failed (see above)".into()) }
}
