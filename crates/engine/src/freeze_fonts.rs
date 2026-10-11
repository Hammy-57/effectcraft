//! The font files and actual loaded faces used by frozen text. All resolution and hashing
//! happens on the capture worker, never in the panel or the UI heartbeat.

use effectcraft_project::{ItemId, LayerId, Project};
use effectcraft_text::fonts;
use std::collections::BTreeSet;

pub fn dependencies(
    project: &Project,
    items: &BTreeSet<ItemId>,
    target: ItemId,
    layers: Option<&BTreeSet<LayerId>>,
    paths: &mut BTreeSet<String>,
) -> Result<(Vec<String>, Option<u64>), String> {
    let mut docs = vec![];
    for id in items {
        let Some(comp) = project.comp(*id) else { continue };
        for layer in &comp.layers {
            if *id == target && layers.is_some_and(|ids| !ids.contains(&layer.id)) {
                continue;
            }
            layer.props.walk("", &mut |_, p| {
                let remaining = 100_001usize.saturating_sub(docs.len());
                docs.extend(std::iter::once(&p.value).chain(p.keys.iter().map(|k| &k.value)).filter_map(|v| v.as_text()).take(remaining));
            });
        }
    }
    if docs.is_empty() {
        return Ok((vec![], None));
    }
    if docs.len() > 100_000 {
        return Err("Smart Freeze text exceeds its verification budget".into());
    }
    // Resolve linked fonts first so capture order cannot change the fallback used by an
    // earlier document that requests the same font without a link.
    for doc in &docs {
        if doc.text.len() > 1_000_000 || doc.runs.len() > 100_000 {
            return Err("Smart Freeze text exceeds its verification budget".into());
        }
        for (font, link, replacement) in std::iter::once((&doc.font, doc.font_file.as_ref(), doc.render_font.as_ref()))
            .chain(doc.runs.iter().map(|r| (&r.style.font, r.style.font_file.as_ref(), r.style.render_font.as_ref())))
        {
            if replacement.is_none()
                && let Some(link) = link
            {
                paths.insert(link.path.clone());
                if fonts::register_linked(font, link).is_none() {
                    return Err(format!("Smart Freeze cannot verify linked font {font}: {}", link.path));
                }
            }
        }
    }
    // Resolution/fallback can lazily scan system fonts. Complete that one-time worker-side
    // initialization before taking the generation rather than rejecting our own first scan.
    fonts::scan_system();
    // Ensure bundled faces are initialized before taking the generation.
    let _ = fonts::resolve(fonts::DEFAULT_FAMILY, "");
    let generation = fonts::revision();
    let mut used = BTreeSet::new();
    let mut work = 0usize;
    for doc in docs {
        let chars: BTreeSet<_> = doc.text.chars().flat_map(|c| std::iter::once(c).chain(c.to_uppercase())).collect();
        for style in std::iter::once(doc.base_style()).chain(doc.runs.iter().map(|r| r.style.clone())) {
            work = work.saturating_add(1);
            if work > 100_000 {
                return Err("Smart Freeze font styles exceed their verification budget".into());
            }
            let face = fonts::resolve_for_render(&style.font, &style.style, style.render_font.as_deref()).0.face;
            used.insert(face);
            for c in &chars {
                work = work.saturating_add(1);
                if work > 100_000 {
                    return Err("Smart Freeze glyph fallback checks exceed their budget".into());
                }
                used.insert(fonts::fallback_for(*c, face));
            }
            if used.len() > 256 {
                return Err("Smart Freeze font dependencies exceed their budget".into());
            }
        }
    }
    let mut signatures = BTreeSet::new();
    for id in used {
        let face = fonts::face(id);
        if let fonts::FaceData::File(path) = &face.info.data {
            paths.insert(path.to_string_lossy().into_owned());
        }
        let hash = face.content_hash().ok_or_else(|| format!("Smart Freeze cannot read font {}", face.info.full_name))?;
        signatures.insert(format!("{}:{}:{}:{hash}", face.info.family, face.info.style, face.info.index));
    }
    if fonts::revision() != generation {
        return Err("Fonts changed during Smart Freeze capture; refresh again".into());
    }
    Ok((signatures.into_iter().collect(), Some(generation)))
}
