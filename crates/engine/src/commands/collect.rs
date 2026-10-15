//! Portable project handoff. Publish a project only after every required media copy succeeds.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use effectcraft_project::{Footage, FootageKind, ItemKind};
use serde_json::{Value, json};

use super::{CommandSpec, bad, str_p};
use crate::{EngineError, Result, Session, cmd};

const COMMAND: &str = "file.collectFiles";

fn enabled(s: &Session) -> std::result::Result<(), String> {
    if cfg!(target_arch = "wasm32") || !s.services.supports_native_collection() {
        Err("Collect Files requires the desktop filesystem".into())
    } else if s.project.items.is_empty() {
        Err("the project is empty".into())
    } else {
        Ok(())
    }
}

// Preserve ordinary sequence names while making Unix-only names portable to Windows.
fn portable_name(raw: &str) -> String {
    let name: String = raw.chars().map(|c| if c.is_control() || "<>:\"/\\|?*".contains(c) { '_' } else { c }).collect();
    let name = name.trim_end_matches(['.', ' ']);
    let stem = name.split('.').next().unwrap_or("").to_ascii_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ["COM", "LPT"].iter().any(|prefix| {
            stem.strip_prefix(prefix).is_some_and(|suffix| matches!(suffix, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"))
        });
    if name.is_empty() || reserved { format!("asset_{name}") } else { name.to_string() }
}

#[derive(Default)]
struct MediaPlan {
    copies: BTreeMap<String, String>,
    directories: BTreeMap<PathBuf, usize>,
    names: BTreeSet<String>,
    embedded: Vec<(String, String)>,
}

impl MediaPlan {
    fn add(&mut self, source: &str) -> Result<String> {
        if let Some(relative) = self.copies.get(source) {
            return Ok(relative.clone());
        }
        let source_path = Path::new(source);
        let basename = source_path.file_name().and_then(|n| n.to_str()).ok_or_else(|| bad(COMMAND, format!("invalid media path: {source}")))?;
        let parent = source_path.parent().unwrap_or_else(|| Path::new(".")).to_path_buf();
        let next = self.directories.len();
        let directory = *self.directories.entry(parent).or_insert(next);
        let name = portable_name(basename);
        let mut relative = format!("(Footage)/{directory}/{name}");
        // A case-sensitive sender may have names that collide on the recipient's filesystem.
        let mut suffix = 0usize;
        while !self.names.insert(relative.to_lowercase()) {
            suffix = suffix.checked_add(1).ok_or_else(|| bad(COMMAND, "too many filename collisions"))?;
            relative = format!("(Footage)/{directory}/{suffix}_{name}");
        }
        self.copies.insert(source.to_string(), relative.clone());
        Ok(relative)
    }

    fn footage(&mut self, f: &mut Footage) -> Result<()> {
        // Expressions consume the saved data. Do not make sharing depend on an obsolete source.
        if f.kind == FootageKind::Data
            && let Some(data) = &f.data
        {
            // data_format() derives CSV/TSV parsing from this extension. Materialize the saved
            // text, not the potentially stale/missing original, so Import can also resolve it.
            let path = format!("(Data)/{}.{}", self.embedded.len(), f.data_format().unwrap_or("json"));
            self.embedded.push((path.clone(), data.clone()));
            f.path = path;
            f.sequence.clear();
            f.missing = false;
            return Ok(());
        }
        if !f.sequence.is_empty() {
            let paths = f.sequence.iter().map(|path| self.add(path)).collect::<Result<Vec<_>>>()?;
            f.path = paths.first().cloned().unwrap_or_default();
            f.sequence = paths;
        } else if !f.path.is_empty() {
            f.path = self.add(&f.path)?;
        } else {
            // Placeholders deliberately have no file and must remain marked missing.
            return Ok(());
        }
        f.missing = false;
        Ok(())
    }
}

fn collect(s: &mut Session, params: &Value) -> Result<Value> {
    enabled(s).map_err(EngineError::Other)?;
    let folder = str_p(params, "folder").filter(|p| !p.trim().is_empty()).ok_or_else(|| bad(COMMAND, "missing `folder` (a new directory)"))?;
    let folder = std::path::absolute(folder).map_err(|e| bad(COMMAND, format!("invalid destination: {e}")))?;
    let mut project = (*s.project).clone();
    // Rebase source paths before replacing them with package-relative paths.
    let source_dir = s.path.as_deref().and_then(|path| Path::new(path).parent()).unwrap_or_else(|| Path::new("."));
    super::import_project::resolve_footage(s, &mut project, source_dir, false);
    let warnings = crate::collect_report::dependency_warnings(&project);
    let mut plan = MediaPlan::default();
    for item in project.items.values_mut() {
        if let ItemKind::Footage(f) = &mut item.kind {
            plan.footage(f)?;
        }
        if let Some(proxy) = &mut item.proxy {
            plan.footage(&mut proxy.footage)?;
        }
    }
    // Caches are local acceleration, never the source of the shared editable document.
    project.freeze.namespace.clear();
    for section in &mut project.freeze.sections {
        section.cache = None;
        section.previous.clear();
    }
    let text = project.to_file_json_with(s.open_comps_to_save().as_ref())?;
    let stem = s.path.as_deref().and_then(|path| Path::new(path).file_stem()).and_then(|s| s.to_str()).unwrap_or("Untitled Project");
    let project_name = format!("{}.ecproj", portable_name(stem));
    let out = folder.join(&project_name).to_string_lossy().into_owned();
    let report = json!({"project": project_name, "copied": plan.copies.len(), "embedded": plan.embedded.len(), "warnings": warnings,
        "notes": "Fonts are not bundled. Review warnings and install the required fonts before editing or rendering."});
    let report_text = serde_json::to_vec_pretty(&report).map_err(|e| EngineError::Other(e.to_string()))?;
    let output = crate::collect_output::NativeOutput::create(&folder, s.services.as_ref())
        .map_err(|e| bad(COMMAND, format!("cannot create {} (choose a new folder): {e}", folder.display())))?;
    let write = || -> Result<()> {
        for (relative, data) in &plan.embedded {
            output.write(relative, data.as_bytes()).map_err(|e| EngineError::Other(format!("cannot package embedded data: {e}")))?;
        }
        for (source, relative) in &plan.copies {
            output.copy(source, relative).map_err(|e| EngineError::Other(format!("cannot collect {source}: {e}")))?;
        }
        output.write("Collection Report.json", &report_text).map_err(|e| EngineError::Other(format!("cannot write collection report: {e}")))?;
        // Last file is the completion marker; failed media copies never publish a project.
        output.write(&project_name, text.as_bytes()).map_err(|e| EngineError::Other(format!("cannot write {out}: {e}")))?;
        Ok(())
    };
    if let Err(error) = write() {
        return match output.abort() {
            Ok(()) => Err(error),
            Err(cleanup) => Err(EngineError::Other(format!("{error}; could not remove incomplete collection {}: {cleanup}", folder.display()))),
        };
    }
    output.commit();
    let copied = plan.copies.len();
    let embedded = plan.embedded.len();
    if warnings.is_empty() {
        s.toast(format!("Collected {copied} media file(s) and {embedded} data snapshot(s) into {}", folder.display()));
    } else {
        s.toast(format!(
            "Collected {copied} media file(s) and {embedded} data snapshot(s) with {} dependency warning(s). Read Collection Report.json in {}",
            warnings.len(),
            folder.display()
        ));
    }
    Ok(
        json!({"project": out, "copied": copied, "embedded": embedded, "errors": [], "warnings": warnings, "report": folder.join("Collection Report.json").to_string_lossy()}),
    )
}

pub fn specs() -> Vec<CommandSpec> {
    vec![cmd!(
        "file.collectFiles",
        "Collect Files...",
        ["File", "Dependencies"],
        None,
        "{folder: string (new desktop directory)} — copies media and proxies, uses relative paths, writes Collection Report.json with unbundled dependencies; fails without a completed project if copying fails.",
        enabled,
        collect
    )]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filenames_preserve_numbered_sequences_and_avoid_windows_devices() {
        assert_eq!(portable_name("shot.0001.png"), "shot.0001.png");
        for name in ["CON.png", "COM1.mov", "LPT³.txt", "NUL", ".", ".."] {
            assert!(portable_name(name).starts_with("asset_"), "{name}");
        }
        assert_eq!(portable_name("a:b?.png "), "a_b_.png");
    }
}
