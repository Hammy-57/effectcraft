//! Clean, editable text fragments positioned from EffectCraft's own shaped glyphs.
use super::{CommandSpec, b_p, bad, has_layers, layers_p, str_p};
use crate::{Result, Session, cmd};
use effectcraft_keyframe::{TextDoc, Value as KV};
use effectcraft_project::{Layer, LayerSource, Node};
use serde_json::{Value, json};

const C: &str = "text.split";
fn fragments(doc: &TextDoc, mode: &str, needle: &str) -> Result<Vec<(TextDoc, [f64; 2])>> {
    if doc.text.len() > 32_768 {
        return Err(bad(C, "Split at most 32 KB of text at a time."));
    }
    let layout = effectcraft_text::layout_doc(doc);
    let source_bytes: Vec<_> = doc.text.char_indices().map(|(b, _)| b).chain(std::iter::once(doc.text.len())).collect();
    let to_source = |byte: usize| -> Result<usize> {
        let ci = layout.byte_of_char.binary_search(&byte).map_err(|_| bad(C, "Invalid shaped boundary."))?;
        source_bytes.get(ci).copied().ok_or_else(|| bad(C, "Invalid shaped boundary."))
    };
    let mut spans = Vec::new();
    match mode {
        "lines" => {
            for l in &layout.layout.lines {
                spans.push(to_source(l.range.start)?..to_source(l.range.end)?);
            }
        }
        "characters" => {
            // A shaped cluster is indivisible: keep combining marks and ligatures together.
            let mut starts: Vec<_> = layout.glyphs.iter().filter_map(|g| source_bytes.get(g.char_index).copied()).collect();
            starts.push(doc.text.len());
            starts.sort_unstable();
            starts.dedup();
            for pair in starts.windows(2) {
                if let [a, b] = pair {
                    spans.push(*a..*b);
                }
            }
        }
        "words" => {
            let mut start = None;
            for (i, ch) in doc.text.char_indices() {
                if ch.is_whitespace() {
                    if let Some(a) = start.take() {
                        spans.push(a..i);
                    }
                } else if start.is_none() {
                    start = Some(i);
                }
            }
            if let Some(a) = start {
                spans.push(a..doc.text.len());
            }
        }
        "custom" => {
            if needle.is_empty() || needle.len() > 1024 {
                return Err(bad(C, "Enter a word or phrase (up to 1024 bytes)."));
            }
            let mut cursor = 0;
            for (a, text) in doc.text.match_indices(needle) {
                if a > cursor {
                    spans.push(cursor..a);
                }
                cursor = a.saturating_add(text.len());
                spans.push(a..cursor);
            }
            if cursor == 0 {
                return Err(bad(C, "That word or phrase is not in the text."));
            }
            if cursor < doc.text.len() {
                spans.push(cursor..doc.text.len());
            }
        }
        _ => return Err(bad(C, "Choose words, characters, lines or custom.")),
    }
    if spans.len() > 256 {
        return Err(bad(C, "This split would create more than 256 layers. Split into lines first."));
    }
    let mut out = Vec::new();
    for span in spans {
        let text = doc.text.get(span.clone()).ok_or_else(|| bad(C, "Invalid text boundary."))?;
        if text.trim().is_empty() {
            continue;
        }
        let before = doc.text.get(..span.start).ok_or_else(|| bad(C, "Invalid text boundary."))?.chars().count();
        let mut part = doc.slice(before..before.saturating_add(text.chars().count()));
        part.box_size = None;
        let piece = effectcraft_text::layout_doc(&part);
        let original: Vec<_> = layout.glyphs.iter().filter(|g| g.char_index >= before && g.char_index < before.saturating_add(text.chars().count())).collect();
        let (Some(a), Some(b)) = (original.first(), piece.glyphs.first()) else { continue };
        let shift = [(a.origin.x - b.origin.x), (a.origin.y - b.origin.y)];
        // Refuse reshaping that cannot be represented by a translated editable text layer.
        // This protects joined scripts, wrap/hyphenation, and cross-fragment ligatures.
        if original.len() != piece.glyphs.len()
            || original.iter().zip(&piece.glyphs).any(|(a, b)| {
                a.gid != b.gid || a.face != b.face || ((a.origin.x - b.origin.x) - shift[0]).abs() > 0.05 || ((a.origin.y - b.origin.y) - shift[1]).abs() > 0.05
            })
        {
            return Err(bad(C, "This split changes text shaping. Try splitting by lines to preserve the appearance."));
        }
        out.push((part, shift));
    }
    if out.is_empty() {
        return Err(bad(C, "The text has no visible fragments."));
    }
    Ok(out)
}
fn id_budget(layer: &Layer) -> Result<u64> {
    let mut stack = vec![(&layer.props, 0usize)];
    let mut nodes = 2u64; // layer id and reid's trailing reservation
    while let Some((group, depth)) = stack.pop() {
        if depth > 64 || nodes > 4096 {
            return Err(bad(C, "Text property tree is too large to split safely."));
        }
        nodes = nodes.saturating_add(1);
        for node in &group.children {
            match node {
                Node::Group(g) => stack.push((g, depth.saturating_add(1))),
                Node::Prop(_) => nodes = nodes.saturating_add(1),
            }
        }
        if stack.len() > 4096 {
            return Err(bad(C, "Text property tree is too large to split safely."));
        }
    }
    if nodes > 4096 {
        return Err(bad(C, "Text property tree is too large to split safely."));
    }
    Ok(nodes)
}
fn prepare(layer: &Layer, mode: &str, needle: &str) -> Result<Vec<Layer>> {
    if !matches!(layer.source, LayerSource::Text) || layer.switches.locked {
        return Err(bad(C, "Select unlocked text layers only."));
    }
    let source = layer.props.prop("text/sourceText").ok_or_else(|| bad(C, "Missing source text."))?;
    if !source.keys.is_empty() || source.expr.is_some() {
        return Err(bad(C, "Animated source text must be made static before splitting."));
    }
    if layer.props.group("text/animators").is_some_and(|g| g.groups().next().is_some())
        || layer.props.group("effects").is_some_and(|g| g.groups().next().is_some())
        || layer.props.group("masks").is_some_and(|g| g.groups().next().is_some())
    {
        return Err(bad(C, "Split before adding text animators, masks or effects."));
    }
    let KV::Text(doc) = &source.value else {
        return Err(bad(C, "Missing text document."));
    };
    let anchor = layer.props.prop("transform/anchor").ok_or_else(|| bad(C, "Missing anchor."))?;
    if anchor.expr.is_some() {
        return Err(bad(C, "Remove the anchor expression before splitting."));
    }
    let mut out = Vec::new();
    for (part, shift) in fragments(doc, mode, needle)? {
        let mut copy = layer.clone();
        copy.name = part.text.chars().take(80).collect();
        copy.props.prop_mut("text/sourceText").ok_or_else(|| bad(C, "Missing source text."))?.value = KV::Text(Box::new(part));
        let anchor = copy.props.prop_mut("transform/anchor").ok_or_else(|| bad(C, "Missing anchor."))?;
        fn offset(v: &mut KV, d: [f64; 2]) -> Result<()> {
            match v {
                KV::Vec3(a) => {
                    a[0] -= d[0];
                    a[1] -= d[1];
                }
                KV::Vec2(a) => {
                    a[0] -= d[0];
                    a[1] -= d[1];
                }
                _ => return Err(bad(C, "Invalid anchor value.")),
            }
            Ok(())
        }
        offset(&mut anchor.value, shift)?;
        for key in &mut anchor.keys {
            offset(&mut key.value, shift)?;
        }
        out.push(copy);
    }
    Ok(out)
}
fn split(s: &mut Session, p: &Value) -> Result<Value> {
    let (cid, ids) = layers_p(s, p)?;
    let mode = str_p(p, "mode").unwrap_or("words");
    let reverse = b_p(p, "reverse").unwrap_or(false);
    let delete = b_p(p, "deleteOriginal").unwrap_or(false);
    let comp = s.project.comp(cid).ok_or(crate::EngineError::NoComp)?;
    if ids.is_empty() || ids.len() > 64 {
        return Err(bad(C, "Select 1–64 text layers."));
    }
    let mut plans = Vec::new();
    let mut count = 0usize;
    let mut reserved = s.project.next_id;
    for id in ids {
        let layer = comp.layer(id).ok_or_else(|| bad(C, "Layer no longer exists."))?;
        if comp.layers.iter().any(|l| l.parent == Some(id) || l.track_matte.is_some_and(|m| m.layer == id)) || layer.track_matte.is_some() {
            return Err(bad(C, "Detach text from parent-controller and matte dependencies before splitting."));
        }
        let per_part = id_budget(layer)?;
        let mut parts = prepare(layer, mode, str_p(p, "text").unwrap_or(""))?;
        reserved = reserved.checked_add(per_part.saturating_mul(parts.len() as u64)).ok_or_else(|| bad(C, "Project ids are exhausted."))?;
        count = count.saturating_add(parts.len());
        if count > 256 {
            return Err(bad(C, "Create at most 256 fragments per action."));
        }
        if reverse {
            parts.reverse();
        }
        plans.push((id, parts));
    }
    let created = s.edit("Split Text", None, |proj, st| {
        let mut next = proj.next_id;
        let mut created = Vec::new();
        let comp = proj.comp_mut(cid).ok_or(crate::EngineError::NoComp)?;
        for (id, parts) in plans {
            let index = comp.layers.iter().position(|l| l.id == id).ok_or_else(|| bad(C, "Layer no longer exists."))?;
            if delete {
                comp.layers.remove(index);
            } else if let Some(l) = comp.layers.get_mut(index) {
                l.switches.video = false;
            }
            for (offset, mut part) in parts.into_iter().enumerate() {
                super::edit::reid(&mut part, &mut next);
                created.push(part.id);
                comp.layers.insert(index.saturating_add(offset), part);
            }
        }
        proj.next_id = next;
        st.selected_layers = created.clone();
        st.selected_props.clear();
        st.selected_keys.clear();
        st.text_edit = None;
        Ok(created)
    })?;
    Ok(
        json!({"layers":created.iter().map(|id|id.0).collect::<Vec<_>>(),"message":format!("Created {} editable text layers. Undo restores the original.",created.len())}),
    )
}
pub fn specs() -> Vec<CommandSpec> {
    vec![cmd!("text.split", "Split Text", [], None, "{layers?, mode?: words|characters|lines|custom, text?, reverse?, deleteOriginal?}", has_layers, split)]
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn split_preserves_layout_and_mixed_styles() {
        let mut doc = TextDoc::plain("Hello world");
        doc.apply_style(6..11, |s| s.size = 40.0);
        let parts = fragments(&doc, "words", "").unwrap();
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[1].0.text, "world");
        assert_eq!(parts[1].0.style_at(0).size, 40.0);
        assert!(parts[1].1[0] > parts[0].1[0]);
    }
    #[test]
    fn split_is_one_undo_and_rejects_bad_modes() {
        let mut s = Session::default();
        s.execute("comp.new", json!({"width":640,"height":360,"duration":5})).unwrap();
        s.execute("layer.newText", json!({"text":"Hello world"})).unwrap();
        assert!(s.execute(C, json!({"mode":"invalid"})).is_err());
        s.execute(C, json!({"mode":"words"})).unwrap();
        assert_eq!(s.active_comp().unwrap().layers.len(), 3);
        s.execute("edit.undo", json!({})).unwrap();
        assert_eq!(s.active_comp().unwrap().layers.len(), 1);
    }
    #[test]
    fn split_render_and_exhausted_ids() {
        let mut s = Session::default();
        s.execute("comp.new", json!({"width":640,"height":360,"duration":5})).unwrap();
        s.execute("layer.newText", json!({"text":"Hello world"})).unwrap();
        s.execute("prop.set", json!({"layer":"#1","path":"transform/position","value":[100,150,0]})).unwrap();
        s.execute("prop.set", json!({"layer":"#1","path":"transform/rotation","value":12})).unwrap();
        s.execute("render.snapshot", json!({"name":"before"})).unwrap();
        s.execute(C, json!({"mode":"words"})).unwrap();
        let comparison = s.execute("render.compare", json!({"a":"before","b":{},"min_psnr":40})).unwrap();
        assert_eq!(comparison["pass"], true, "{comparison}");
        assert!(comparison["ssim"].as_f64().unwrap() > 0.999, "{comparison}");
        s.execute("edit.undo", json!({})).unwrap();
        std::sync::Arc::make_mut(&mut s.project).next_id = u64::MAX;
        assert!(s.execute(C, json!({"mode":"words"})).is_err());
        assert_eq!(s.active_comp().unwrap().layers.len(), 1);
    }
    #[test]
    fn split_limits_and_unicode() {
        assert!(fragments(&TextDoc::plain(&"a ".repeat(257)), "words", "").is_err());
        let parts = fragments(&TextDoc::plain("été café"), "words", "").unwrap();
        assert_eq!(parts[1].0.text, "café");
        assert!(fragments(&TextDoc::plain("Hello"), "custom", "").is_err());
    }
}
