//! Library exchange: write selected layers or a comp to a small `.ecproj` that a library (the
//! Motion Library) keeps, and bring saved layers back into any comp. Export never changes the
//! open project; import is one undo step.

use std::collections::BTreeSet;

use effectcraft_color::Label;
use effectcraft_project::{ItemId, ItemKind, Layer, LayerId, Project, essential};
use effectcraft_time::Tick;
use serde_json::{Value, json};

use super::file_more::used_items;
use super::import_project::{read_project, resolve_footage, root_comps};
use super::{CommandSpec, bad, comp_id, f_p, has_comp, layers_p, str_p};
use crate::{EngineError, Result, Session, cmd};

fn write_project(s: &Session, cmd: &str, path: &str, sub: &Project) -> Result<usize> {
    let text = sub.to_file_json_with(None)?;
    let text = if crate::xml_project::is_xml_path(path) { crate::xml_project::to_xml(&text).map_err(EngineError::Other)? } else { text };
    s.services.write_file(path, text.as_bytes()).map_err(|e| bad(cmd, format!("cannot write {path}: {e}")))?;
    Ok(text.len())
}

/// `sub` cut down to `roots` and everything they use, with folders flattened away so the file
/// holds only what an import needs.
fn keep_only(sub: &mut Project, roots: &[ItemId]) {
    let used = used_items(sub, roots);
    sub.items.retain(|id, it| used.contains(id) && !matches!(it.kind, ItemKind::Folder));
    for it in sub.items.values_mut() {
        it.parent = None;
    }
    sub.render_queue.clear();
}

fn export_layers(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "file.exportLayers";
    let path = str_p(p, "path").ok_or_else(|| bad(C, "missing `path` (.ecproj)"))?.to_string();
    let (cid, ids) = layers_p(s, p)?;
    if ids.is_empty() {
        return Err(bad(C, "select layers to export"));
    }
    let comp = s.project.comp(cid).ok_or(EngineError::NoComp)?;
    // The selected layers plus the parents and track mattes they need, transitively.
    let mut keep: BTreeSet<LayerId> = ids.iter().copied().collect();
    loop {
        let more: Vec<LayerId> = comp
            .layers
            .iter()
            .filter(|l| keep.contains(&l.id))
            .flat_map(|l| l.parent.into_iter().chain(l.track_matte.map(|m| m.layer)))
            .filter(|id| !keep.contains(id) && comp.layer(*id).is_some())
            .collect();
        if more.is_empty() {
            break;
        }
        keep.extend(more);
    }
    let saved: Vec<Value> =
        comp.layers.iter().enumerate().filter(|(_, l)| ids.contains(&l.id)).map(|(i, l)| json!({"index": i + 1, "id": l.id.0, "name": l.name})).collect();
    // The time the saved layers span (comp seconds), for a library preview.
    let chosen = || comp.layers.iter().filter(|l| ids.contains(&l.id));
    let start = chosen().map(|l| l.in_point).min().unwrap_or(Tick::ZERO).seconds();
    let end = chosen().map(|l| l.out_point).max().unwrap_or(Tick::ZERO).seconds();
    let frame_rate = comp.frame_rate.as_f64();
    let comp_name = s.project.item(cid).map(|i| i.name.clone()).unwrap_or_default();
    let mut sub: Project = (*s.project).clone();
    if let Some(c) = sub.comp_mut(cid) {
        c.layers.retain(|l| keep.contains(&l.id));
        c.essential = None;
    }
    keep_only(&mut sub, &[cid]);
    let bytes = write_project(s, C, &path, &sub)?;
    Ok(json!({
        "path": path, "comp": comp_name, "layers": saved, "included": keep.len(), "items": sub.items.len(), "bytes": bytes,
        "start": start, "end": end, "frameRate": frame_rate,
    }))
}

fn export_comp(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "file.exportComp";
    let path = str_p(p, "path").ok_or_else(|| bad(C, "missing `path` (.ecproj)"))?.to_string();
    let cid = comp_id(s, p)?;
    let comp_name = s.project.item(cid).map(|i| i.name.clone()).unwrap_or_default();
    let comp = s.project.comp(cid).ok_or(EngineError::NoComp)?;
    let (start, end, frame_rate) = (comp.work_area.0.seconds(), comp.work_area.1.seconds(), comp.frame_rate.as_f64());
    let mut sub: Project = (*s.project).clone();
    keep_only(&mut sub, &[cid]);
    let bytes = write_project(s, C, &path, &sub)?;
    Ok(json!({"path": path, "comp": comp_name, "items": sub.items.len(), "bytes": bytes, "start": start, "end": end, "frameRate": frame_rate}))
}

fn import_layers(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "file.importLayers";
    let path = str_p(p, "path").ok_or_else(|| bad(C, "missing `path` (.ecproj from file.exportLayers)"))?.to_string();
    let cid = comp_id(s, p)?;
    let (mut sub, _) = read_project(s.services.as_ref(), &path)?;
    crate::upgrade_effects(&mut sub);
    let dir = std::path::Path::new(&path).parent().map(|d| d.to_path_buf()).unwrap_or_default();
    let missing = resolve_footage(s, &mut sub, &dir, true);
    let base = s.project.next_id;
    if sub.next_id.checked_add(base).is_none() {
        return Err(bad(C, format!("{path} has ids out of range")));
    }
    essential::offset_ids(&mut sub, base);
    let src = match str_p(p, "from") {
        Some(name) => sub.comps().find(|(id, _)| sub.item(**id).is_some_and(|i| i.name == name)).map(|(id, _)| *id),
        None => match root_comps(&sub).as_slice() {
            [one] => Some(*one),
            _ => None,
        },
    }
    .ok_or_else(|| bad(C, "pass `from` (the comp holding the layers): the file has no single top comp"))?;
    let layers: Vec<Layer> = sub.comp(src).map(|c| c.layers.iter().cloned().collect()).unwrap_or_default();
    if layers.is_empty() {
        return Err(bad(C, format!("{path} holds no layers")));
    }
    sub.items.remove(&src);
    let at = f_p(p, "at").map(Tick::from_seconds_f64).unwrap_or_else(|| s.time());
    let first = layers.iter().map(|l| l.in_point).min().unwrap_or(Tick::ZERO);
    let folder_name =
        format!("{} assets", std::path::Path::new(&path).file_stem().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "Imported".into()));
    let created = s.edit("Import Layers", None, |proj, st| {
        if !sub.items.is_empty() {
            let folder = proj.add_item(&folder_name, Label::Yellow, None, ItemKind::Folder);
            for (id, mut it) in std::mem::take(&mut sub.items) {
                if it.parent.is_none() {
                    it.parent = Some(folder);
                }
                proj.items.insert(id, it);
            }
        }
        proj.next_id = proj.next_id.max(sub.next_id);
        proj.fix_next_id();
        let comp = proj.comp_mut(cid).ok_or(EngineError::NoComp)?;
        let mut created = vec![];
        for (k, mut l) in layers.into_iter().enumerate() {
            l.start_time = l.start_time - first + at;
            l.in_point = l.in_point - first + at;
            l.out_point = l.out_point - first + at;
            l.name = comp.unique_layer_name(&l.name);
            created.push(l.id);
            comp.layers.insert(k, l);
        }
        st.selected_layers = created.clone();
        Ok(created)
    })?;
    Ok(json!({"layers": created.iter().map(|l| l.0).collect::<Vec<_>>(), "missingMedia": missing}))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(
            "file.exportLayers",
            "Export Layers",
            [],
            None,
            "{path (.ecproj), comp?, layers?} — writes the selected layers (with the parents and track mattes they need, and the items they use) to a small project; the open project is unchanged. Returns {path, comp, layers: [{index, id, name}], included, items, bytes, start, end (s, the selected layers' span), frameRate}",
            has_comp,
            export_layers
        ),
        cmd!(
            "file.exportComp",
            "Export Composition",
            [],
            None,
            "{path (.ecproj), comp?} — writes the comp and everything it uses to a small project; the open project is unchanged. Returns {path, comp, items, bytes, start, end (s, the work area), frameRate}",
            has_comp,
            export_comp
        ),
        cmd!(
            "file.importLayers",
            "Import Layers",
            [],
            None,
            "{path (.ecproj from file.exportLayers), comp? (target, default active), from? (source comp name), at? (s, default playhead)} — adds the saved layers at the top of the comp, starting at `at`, with the items they use in a folder; one undo step. Returns {layers, missingMedia}",
            has_comp,
            import_layers
        ),
    ]
}
