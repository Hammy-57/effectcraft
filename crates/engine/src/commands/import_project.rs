//! File ▸ Import ▸ EffectCraft Project: merge another `.ecproj` (or its XML copy) into the
//! current project, as After Effects imports an `.aep` into the open project. The imported
//! items land in a folder named after the file; every id is shifted past the current project's
//! so nothing collides, and the whole import is one undo step.

use effectcraft_color::Label;
use effectcraft_project::{ItemId, ItemKind, LayerSource, Project, essential};
use serde_json::{Value, json};

use super::{CommandSpec, always, b_p, bad, str_p};
use crate::{EngineError, Result, Session, cmd};

const C: &str = "file.importProject";

/// Whether `path` names a project EffectCraft imports as a project (not as footage).
pub(crate) fn is_project_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.ends_with(".ecproj") || lower.ends_with(".ecprojx")
}

/// Read a project file (JSON or its XML copy), as File ▸ Open does.
pub(super) fn read_project(services: &dyn crate::Services, path: &str) -> Result<(Project, Option<String>)> {
    let bytes = services.read_file(path).map_err(|e| EngineError::Other(format!("cannot read {path}: {e}")))?;
    let text = String::from_utf8(bytes).map_err(|_| bad(C, format!("{path} is not a text project file")))?;
    let text = if crate::xml_project::is_xml(&text) { crate::xml_project::from_xml(&text).map_err(EngineError::Other)? } else { text };
    let project = Project::from_json(&text).map_err(|e| bad(C, format!("{path} is not a project EffectCraft can open: {e}")))?;
    Ok((project, effectcraft_project::saved_by(&text)))
}

/// Resolve media against its owning project, never the app's working directory.
/// Keep missing relative paths anchored too, so relinking cannot find an unrelated CWD file.
/// Ordinary open leaves existence checks to its background job (`check_missing: false`).
pub(super) fn resolve_footage(s: &Session, project: &mut Project, dir: &std::path::Path, check_missing: bool) -> Vec<String> {
    let dir = if dir.as_os_str().is_empty() { std::path::Path::new(".") } else { dir };
    let dir = std::path::absolute(dir).unwrap_or_else(|_| dir.to_path_buf());
    let resolve = |raw: &mut String| -> bool {
        if raw.is_empty() {
            return true;
        }
        if !std::path::Path::new(raw.as_str()).is_absolute() {
            *raw = dir.join(raw.as_str()).to_string_lossy().into_owned();
        }
        !check_missing || s.services.exists(raw)
    };
    let mut missing = vec![];
    for it in project.items.values_mut() {
        let footage = match &mut it.kind {
            ItemKind::Footage(f) => Some(f),
            _ => None,
        };
        for f in footage.into_iter().chain(it.proxy.as_mut().map(|p| &mut p.footage)) {
            let present = resolve(&mut f.path);
            let mut all_frames = true;
            for frame in &mut f.sequence {
                all_frames &= resolve(frame);
            }
            if (!present && f.sequence.is_empty()) || !all_frames {
                missing.push(f.path.clone());
            }
        }
    }
    missing
}

/// The comps no other comp of `project` nests: the ones to open or place.
pub(super) fn root_comps(project: &Project) -> Vec<ItemId> {
    let nested: std::collections::HashSet<ItemId> =
        project.comps().flat_map(|(_, c)| c.layers.iter().filter_map(|l| if let LayerSource::Comp { item } = l.source { Some(item) } else { None })).collect();
    project.comps().map(|(id, _)| *id).filter(|id| !nested.contains(id)).collect()
}

fn import_project(s: &mut Session, p: &Value) -> Result<Value> {
    let path = str_p(p, "path").ok_or_else(|| bad(C, "missing `path` (.ecproj)"))?.to_string();
    // `wait: false`: read and parse the file (the slow part for a big project) on a background
    // thread, then merge it on the session's thread, as one undo step.
    if b_p(p, "wait") == Some(false) {
        let services = s.services.clone();
        let name = std::path::Path::new(&path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.clone());
        return s.spawn_task("importProject", format!("Importing {name}"), false, move |_ctl| {
            let (sub, saved_by) = read_project(services.as_ref(), &path).map_err(|e| e.to_string())?;
            let apply: crate::jobs::Apply = Box::new(move |s: &mut Session| merge(s, &path, sub, saved_by));
            Ok(apply)
        });
    }
    let (sub, saved_by) = read_project(s.services.as_ref(), &path)?;
    merge(s, &path, sub, saved_by)
}

/// Merge `sub` (read from `path`) into the open project, as one undo step.
fn merge(s: &mut Session, path: &str, mut sub: Project, saved_by: Option<String>) -> Result<Value> {
    let path = path.to_string();
    crate::upgrade_effects(&mut sub);
    let file = std::path::Path::new(&path);
    let dir = file.parent().map(|d| d.to_path_buf()).unwrap_or_default();
    let missing = resolve_footage(s, &mut sub, &dir, true);
    // The imported project's own render queue stays behind (as for templates).
    let base = s.project.next_id;
    if sub.next_id.checked_add(base).is_none() {
        return Err(bad(C, format!("{path} has ids out of range")));
    }
    essential::offset_ids(&mut sub, base);
    let roots: Vec<ItemId> = root_comps(&sub);
    let folder_name = file.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "Imported Project".into());
    let (folder, items) = s.edit("Import Project", None, |proj, st| {
        let folder = proj.add_item(&folder_name, Label::Yellow, None, ItemKind::Folder);
        let mut items = vec![];
        for (id, mut it) in std::mem::take(&mut sub.items) {
            if it.parent.is_none() {
                it.parent = Some(folder);
            }
            items.push(id.0);
            proj.items.insert(id, it);
        }
        proj.next_id = proj.next_id.max(sub.next_id);
        proj.fix_next_id();
        st.project_selection = vec![folder];
        Ok((folder, items))
    })?;
    let comps: Vec<Value> = s
        .project
        .comps()
        .filter(|(id, _)| items.contains(&id.0))
        .map(|(id, _)| json!({"id": id.0, "name": s.project.item(*id).map(|i| i.name.clone()).unwrap_or_default()}))
        .collect();
    let primary = if let [one] = roots.as_slice() { Some(one.0) } else { None };
    let mut warnings = vec![];
    if let Some(v) = saved_by.as_deref().filter(|v| effectcraft_project::is_newer_version(v, effectcraft_project::APP_VERSION)) {
        warnings.push(format!("saved by EffectCraft {v}, newer than this version ({})", effectcraft_project::APP_VERSION));
    }
    if !missing.is_empty() {
        warnings.push(format!("{} footage file{} missing", missing.len(), if missing.len() == 1 { " is" } else { "s are" }));
    }
    s.toast(if warnings.is_empty() { format!("Imported {folder_name}") } else { format!("Imported {folder_name}: {}", warnings.join("; ")) });
    Ok(json!({
        "folder": folder.0,
        "items": items,
        "comps": comps,
        "rootComps": roots.iter().map(|id| id.0).collect::<Vec<_>>(),
        "primaryComp": primary,
        "missingMedia": missing,
        "warnings": warnings,
    }))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![cmd!(
        "file.importProject",
        "EffectCraft Project...",
        ["File", "Import"],
        None,
        "{path (.ecproj or .ecprojx), wait? (false: read it in the background)} — merges the project into a folder named after the file; returns {folder, items, comps, rootComps, primaryComp (the only root comp, else null), missingMedia, warnings}",
        always,
        import_project
    )]
}
