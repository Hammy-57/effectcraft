//! `cargo xtask parity [--check] [--out FILE]`: check the parts of `docs/parity/` and assemble
//! them into one page (default `<target>/parity.md`). See `docs/parity.md` for the layout: the
//! parts are separate files so parallel branches don't edit one shared document.

use std::path::{Path, PathBuf};

pub fn run(args: &[String]) -> Result<(), String> {
    let check = args.iter().any(|a| a == "--check");
    let out = args.iter().position(|a| a == "--out").and_then(|i| args.get(i + 1)).map(PathBuf::from);
    let dir = crate::root().join("docs/parity");
    let page = assemble(&dir)?;
    if check {
        println!("parity: ok");
        return Ok(());
    }
    let out = out.unwrap_or_else(|| crate::target_dir().join("parity.md"));
    if let Some(parent) = out.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    std::fs::write(&out, page).map_err(|e| format!("{}: {e}", out.display()))?;
    println!("parity: wrote {}", out.display());
    Ok(())
}

fn read(path: &Path) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))
}

/// `.md` files of `dir`, in natural order (`m3.9` before `m13.5`).
fn parts(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "md"))
        .collect();
    v.sort_by_key(|p| natural_key(&p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()));
    Ok(v)
}

/// Digit runs compare as numbers.
fn natural_key(s: &str) -> Vec<(u64, String)> {
    let mut out = vec![];
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c.is_ascii_digit() {
            let mut n = c.to_digit(10).map_or(0, u64::from);
            while let Some(d) = chars.peek().and_then(|d| d.to_digit(10)) {
                n = n.saturating_mul(10).saturating_add(u64::from(d));
                chars.next();
            }
            out.push((n, String::new()));
        } else {
            out.push((u64::MAX, c.to_string()));
        }
    }
    out
}

/// One row of the per-area table from `areas/<area>.md`:
/// `# Name`, `- Weighted parity: …`, `- Remaining (agent-hours): …`, then the biggest gaps.
fn area_row(path: &Path) -> Result<String, String> {
    let text = read(path)?;
    let err = |what: &str| format!("{}: {what} (expected `# Area`, `- Weighted parity: …`, `- Remaining (agent-hours): …`, then the gaps)", path.display());
    let mut lines = text.lines();
    let name = lines.next().and_then(|l| l.strip_prefix("# ")).ok_or_else(|| err("no `# Area` heading"))?.trim();
    let (mut weighted, mut remaining, mut gaps) = (None, None, vec![]);
    for l in lines {
        if let Some(v) = l.strip_prefix("- Weighted parity:") {
            weighted = Some(v.trim());
        } else if let Some(v) = l.strip_prefix("- Remaining (agent-hours):") {
            remaining = Some(v.trim());
        } else if !l.trim().is_empty() {
            gaps.push(l.trim());
        }
    }
    let (Some(w), Some(r)) = (weighted, remaining) else { return Err(err("missing a field")) };
    let gaps = gaps.join(" ");
    if gaps.is_empty() || [name, w, r, gaps.as_str()].iter().any(|c| c.contains('|')) {
        return Err(err("empty gaps, or a `|` in a cell"));
    }
    Ok(format!("| {name} | {w} | {r} | {gaps} |\n"))
}

/// The whole page: overview, original audit, per-area table, effects, updates (natural order),
/// gaps. Links stay as the parts write them (relative to `docs/parity/`).
fn assemble(dir: &Path) -> Result<String, String> {
    let mut page =
        String::from("# Parity with After Effects\n\n<!-- Assembled by `cargo xtask parity` from docs/parity/; edit the parts, not this page. -->\n\n");
    let mut push = |text: &str| {
        page.push_str(text.trim_end());
        page.push_str("\n\n");
    };
    push(&read(&dir.join("overview.md"))?);
    push(&read(&dir.join("original-audit.md"))?);
    let mut table = String::from("## By area\n\n| Area | Weighted parity | Remaining (agent-hours) | Biggest gaps |\n|---|---|---|---|\n");
    for p in parts(&dir.join("areas"))? {
        table.push_str(&area_row(&p)?);
    }
    push(&table);
    push(&read(&dir.join("effects.md"))?);
    for p in parts(&dir.join("updates"))? {
        let text = read(&p)?;
        if !text.starts_with("## ") {
            return Err(format!("{}: an update starts with a `## Update: <task> <title>` heading", p.display()));
        }
        push(&text);
    }
    push(&read(&dir.join("gaps.md"))?);
    Ok(page.trim_end().to_string() + "\n")
}

#[cfg(test)]
mod tests {
    #[test]
    fn natural_order() {
        let mut v = vec!["m13.05-a.md", "m3.09-b.md", "m4.05-c.md", "m3.10-d.md"];
        v.sort_by_key(|s| super::natural_key(s));
        assert_eq!(v, ["m3.09-b.md", "m3.10-d.md", "m4.05-c.md", "m13.05-a.md"]);
    }

    #[test]
    fn the_parts_assemble() {
        let page = super::assemble(&crate::root().join("docs/parity")).unwrap();
        assert!(page.contains("## Current status"));
        assert!(page.contains("| Paint | 0% | 4.0 | Brush, Clone Stamp, Eraser |"));
    }
}
