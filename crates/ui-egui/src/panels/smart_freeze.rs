//! Native Smart Freeze panel. Every action goes through the command registry.

use egui::{Align2, Rect, pos2, vec2};
use serde_json::json;

use super::panel_kit as kit;
use crate::{EffectcraftApp, theme::Tokens};

/// Visual indicators only: never rewrite source names, labels or switches.
pub fn timeline_badges(
    app: &EffectcraftApp,
    comp: effectcraft_engine::project::ItemId,
) -> std::collections::BTreeMap<effectcraft_engine::project::LayerId, &'static str> {
    use effectcraft_engine::project::{ItemKind, LayerSource, freeze::Target};
    let mut badges = std::collections::BTreeMap::new();
    if app.frames.remote_active() || cfg!(target_arch = "wasm32") {
        return badges;
    }
    let project = app.session.freeze_preview_project();
    let Some(c) = project.comp(comp) else { return badges };
    for section in project.freeze.sections.iter().filter(|s| s.enabled) {
        let Some(version) = &section.cache else { continue };
        let label = if version.draft { "DRAFT" } else { "FROZEN" };
        for layer in &c.layers {
            let visual = match layer.source {
                LayerSource::Null | LayerSource::Camera | LayerSource::Light { .. } => false,
                LayerSource::Footage { item } => matches!(project.item(item).map(|i|&i.kind),Some(ItemKind::Footage(f)) if f.has_video),
                _ => true,
            };
            if !visual {
                continue;
            }
            let direct = section.target.comp() == comp
                && match &section.target {
                    Target::Selection { layers, .. } => layers.contains(&layer.id),
                    _ => true,
                };
            let source = matches!(layer.source,LayerSource::Comp {item} if item==section.target.comp()) && !matches!(section.target, Target::Selection { .. });
            if direct || source {
                badges.insert(layer.id, label);
            }
        }
    }
    badges
}

pub fn show(app: &mut EffectcraftApp, ui: &mut egui::Ui, rect: Rect) {
    let scroll = crate::widgets::PanelScroll::begin(ui, egui::Id::new("smart-freeze-scroll"), rect);
    let old_clip = ui.clip_rect();
    ui.set_clip_rect(old_clip.intersect(rect));
    let t = app.tokens;
    let painter = ui.painter().with_clip_rect(rect);
    painter.rect_filled(rect, 0.0, t.panel_bg);
    let x = rect.min.x + 10.0;
    let width = (rect.width() - 20.0).max(1.0);
    let start = rect.min.y + 10.0 - scroll.offset;
    let mut y = start;
    painter.text(pos2(x, y), Align2::LEFT_TOP, "Keep the motion. Edit the originals anytime.", Tokens::ui(11.0), t.text_dim);
    y += 24.0;
    if kit::button(app, ui, Rect::from_min_size(pos2(x, y), vec2(width, 24.0)), "smartFreeze.freeze", "Freeze Selection / Composition", false) {
        kit::exec(app, "freeze.freeze", json!({}));
    }
    y += 32.0;
    if kit::button(app, ui, Rect::from_min_size(pos2(x, y), vec2(width, 22.0)), "smartFreeze.originals", "Use Originals / Stop Auto", false) {
        kit::exec(app, "freeze.originals", json!({}));
    }
    y += 30.0;
    let sections = app.session.project.freeze.sections.clone();
    if sections.is_empty() {
        painter.text(pos2(x, y), Align2::LEFT_TOP, "No frozen sections yet.", Tokens::ui(12.0), t.text_faint);
        y += 22.0;
    }
    for section in sections {
        if kit::button(app, ui, Rect::from_min_size(pos2(x, y), vec2(width, 22.0)), &format!("smartFreeze.section.{}", section.id), &section.name, false) {
            kit::exec(app, "comp.open", json!({"comp": section.target.comp().0}));
        }
        y += 25.0;
        let status = if !section.enabled {
            "LIVE"
        } else if section.cache.as_ref().is_some_and(|v| app.session.freeze_runtime.bad_versions.contains(&v.directory)) {
            app.session.freeze_runtime.reasons.get(&section.id).map(String::as_str).unwrap_or("Cache unavailable · ORIGINALS LIVE")
        } else if section.auto {
            if let Some(reason) = app.session.freeze_runtime.reasons.get(&section.id) {
                reason.as_str()
            } else if app.session.freeze_runtime.proofs.get(&section.id).is_some_and(|p| p.revision == app.session.revision) {
                if section.cache.as_ref().is_some_and(|c| c.draft) { "DRAFT · AUTO" } else { "FROZEN · AUTO" }
            } else {
                "CHECKING · ORIGINALS LIVE"
            }
        } else if section.cache.as_ref().is_some_and(|c| c.draft) {
            "DRAFT · MANUAL (unchecked)"
        } else if section.cache.is_some() {
            "FROZEN · MANUAL (unchecked)"
        } else {
            "NOT FROZEN"
        };
        painter.text(pos2(x, y), Align2::LEFT_TOP, status, Tokens::ui(10.5), t.text_dim);
        y += 20.0;
        let half = (width - 6.0) / 2.0;
        if kit::button(app, ui, Rect::from_min_size(pos2(x, y), vec2(half, 22.0)), &format!("smartFreeze.edit.{}", section.id), "Edit Originals", false) {
            kit::exec(app, "freeze.edit", json!({"section": section.id}));
        }
        if kit::button(
            app,
            ui,
            Rect::from_min_size(pos2(x + half + 6.0, y), vec2(half, 22.0)),
            &format!("smartFreeze.refresh.{}", section.id),
            "Refresh",
            false,
        ) {
            kit::exec(app, "freeze.refresh", json!({"section": section.id}));
        }
        y += 33.0;
        let auto = kit::checkbox(app, ui, pos2(x, y), &format!("smartFreeze.auto.{}", section.id), "Auto-freeze after 8 seconds idle", section.auto);
        if auto != section.auto {
            kit::exec(app, "freeze.auto", json!({"section":section.id, "enabled":auto}));
        }
        y += 27.0;
        let draft = kit::checkbox(app, ui, pos2(x, y), &format!("smartFreeze.draft.{}", section.id), "Quick draft, then full quality", section.quick_draft);
        if draft != section.quick_draft {
            kit::exec(app, "freeze.configure", json!({"section":section.id,"quickDraft":draft}));
        }
        y += 27.0;
        if let Some(sample) = section.cache.as_ref().and_then(|v| v.sample.as_ref()) {
            painter.text(
                pos2(x, y),
                Align2::LEFT_TOP,
                format!("Sample: live {:.1} ms · cache {:.1} ms", sample.live_ms, sample.cached_ms),
                Tokens::ui(10.5),
                t.text_dim,
            );
            y += 22.0;
            if section.cache.as_ref().is_some_and(|v| sample.worth_freezing(v.frame_rate))
                && app.session.freeze_runtime.changed.elapsed().as_secs_f64() >= 30.0f64.max(sample.render_seconds * 3.0)
            {
                painter.text(pos2(x, y), Align2::LEFT_TOP, "Good Auto candidate: expensive and rarely edited", Tokens::ui(10.5), t.accent);
                y += 22.0;
            }
        }
    }
    let jobs: Vec<_> = app.session.jobs().into_iter().filter(|j| j.kind == "smartFreeze").collect();
    for job in jobs {
        painter.text(pos2(x, y), Align2::LEFT_TOP, format!("{} · {:.0}%", job.label, job.fraction.unwrap_or(0.0) * 100.0), Tokens::ui(11.0), t.accent);
        y += 20.0;
        if kit::button(app, ui, Rect::from_min_size(pos2(x, y), vec2(width, 22.0)), "smartFreeze.cancel", "Cancel Render", false) {
            kit::exec(app, "freeze.cancel", json!({}));
        }
        y += 30.0;
    }
    for job in app.session.job_log.iter().rev().filter(|j| j.kind == "smartFreeze" && j.status != "done").take(3) {
        painter.text(pos2(x, y), Align2::LEFT_TOP, &job.message, Tokens::ui(10.5), t.danger);
        y += 22.0;
    }
    y += 10.0;
    painter.text(pos2(x, y), Align2::LEFT_TOP, "Rendering & storage", Tokens::semibold(12.0), t.text);
    y += 24.0;
    let storage = app
        .session
        .job_log
        .iter()
        .rev()
        .find(|j| j.kind == "freezeStorage" && j.result["namespace"].as_str() == Some(app.session.project.freeze.namespace.as_str()))
        .map(|j| j.result.clone());
    let budget = app.session.project.freeze.budget_bytes as f64 / (1024.0 * 1024.0 * 1024.0);
    let used = storage
        .as_ref()
        .and_then(|v| v["storage"]["bytes"].as_u64())
        .map(|b| format!("{:.2} GiB", b as f64 / (1024.0 * 1024.0 * 1024.0)))
        .unwrap_or_else(|| "Not measured".into());
    painter.text(pos2(x, y), Align2::LEFT_TOP, format!("{used} · soft budget {budget:.0} GiB"), Tokens::ui(11.0), t.text_dim);
    y += 22.0;
    let mut edited_budget = budget;
    let field = Rect::from_min_size(pos2(x, y), vec2(width.min(140.0), 22.0));
    let response = ui.put(field, egui::DragValue::new(&mut edited_budget).range(0.0..=8192.0).suffix(" GiB"));
    app.auto.add("smartFreeze.budget", field, "Soft cache budget in GiB");
    if response.changed() {
        kit::exec(app, "freeze.settings", json!({"budgetGiB":edited_budget}));
    }
    y += 30.0;
    let retire = app.session.project.freeze.retire_old;
    let changed = kit::checkbox(app, ui, pos2(x, y), "smartFreeze.retirement", "Retire old versions after 10 hours", retire);
    if changed != retire {
        kit::exec(app, "freeze.settings", json!({"retireOld":changed}));
    }
    y += 27.0;
    for (id, label, command, params) in [
        ("smartFreeze.measure", "Measure Storage", "freeze.storage", json!({})),
        ("smartFreeze.cleanup", "Clean Abandoned Jobs", "freeze.cleanup", json!({})),
        ("smartFreeze.retireNow", "Move Previous Versions to Trash Now", "freeze.cleanup", json!({"retireNow":true})),
    ] {
        if kit::button(app, ui, Rect::from_min_size(pos2(x, y), vec2(width, 22.0)), id, label, false) {
            kit::exec(app, command, params);
        }
        y += 29.0;
    }
    if let Some(storage) = storage {
        for (index, version) in storage["versions"].as_array().into_iter().flatten().filter(|v| v["trash_path"].is_string()).take(3).enumerate() {
            if kit::button(
                app,
                ui,
                Rect::from_min_size(pos2(x, y), vec2(width, 22.0)),
                &format!("smartFreeze.restore.{index}"),
                "Restore Previous Version from Trash",
                false,
            ) {
                kit::exec(app, "freeze.restore", json!({"directory":version["version"]["directory"]}));
            }
            y += 29.0;
        }
        if let Some(message) = storage["storage"]["errors"].as_array().and_then(|v| v.first()).and_then(|v| v.as_str()) {
            painter.text(pos2(x, y), Align2::LEFT_TOP, message, Tokens::ui(10.5), t.danger);
            y += 24.0;
        }
    }
    ui.set_clip_rect(old_clip);
    scroll.end(ui, &mut app.auto, "smartFreeze.scroll", y - start + 20.0, &t);
}
