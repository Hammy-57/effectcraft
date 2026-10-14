//! Instance-aware projections of real nested layers; no document copies or render changes.
use super::*;
use effectcraft_engine::commands::inline_comp::{self, Context};

const MAX_ROWS: usize = 20_000;

#[derive(Clone, Debug)]
pub(super) struct InlineRow {
    root: ItemId,
    path: Vec<u64>,
    context: Context,
    row: Row,
}

fn instance_key(root: ItemId, path: &[u64]) -> Vec<u64> {
    std::iter::once(root.0).chain(path.iter().copied()).collect()
}

pub(super) fn expander(app: &mut EffectcraftApp, ui: &mut egui::Ui, root: ItemId, path: &[u64], layer: &Layer, rect: Rect) {
    if !matches!(layer.source, LayerSource::Comp { .. }) {
        return;
    }
    let mut child_path = path.to_vec();
    child_path.push(layer.id.0);
    let key = instance_key(root, &child_path);
    let open = app.ui.timeline.inline_precomps.contains(&key);
    let resp = widgets::twirl(ui, rect, open, egui::Id::new(("inline-precomp", &key)), &app.tokens)
        .on_hover_text("Expand this composition's layers inline (edits affect every instance)");
    app.auto.add(&format!("timeline.inline.{}.expand", key.iter().map(u64::to_string).collect::<Vec<_>>().join(".")), rect, "Expand nested composition");
    if resp.clicked() {
        if open {
            app.ui.timeline.inline_precomps.remove(&key);
        } else if let Err(e) = inline_comp::context(&app.session, root, &child_path, app.session.time_of(root)) {
            app.ui.status = e.to_string();
        } else {
            app.ui.timeline.inline_precomps.insert(key);
        }
    }
}

pub(super) fn expand(app: &EffectcraftApp, root: ItemId, rows: Vec<Row>) -> Vec<Row> {
    let mut out = Vec::with_capacity(rows.len());
    let mut i = 0;
    while let Some(row) = rows.get(i) {
        let layer = row.layer;
        while let Some(row) = rows.get(i).filter(|r| r.layer == layer) {
            out.push(row.clone());
            i += 1;
        }
        append(app, root, &[layer.0], &mut out);
    }
    out
}

fn append(app: &EffectcraftApp, root: ItemId, path: &[u64], out: &mut Vec<Row>) {
    if out.len() >= MAX_ROWS || !app.ui.timeline.inline_precomps.contains(&instance_key(root, path)) {
        return;
    }
    let Ok(context) = inline_comp::context(&app.session, root, path, app.session.time_of(root)) else { return };
    let Some(comp) = app.session.project.comp(context.comp) else { return };
    let rows = build_rows(app, comp);
    let mut i = 0;
    while let Some(row) = rows.get(i) {
        if out.len() >= MAX_ROWS {
            return;
        }
        let layer = row.layer;
        while let Some(row) = rows.get(i).filter(|r| r.layer == layer) {
            if out.len() >= MAX_ROWS {
                return;
            }
            // Waveforms use a separate source cache; keep them in the source comp timeline.
            if matches!(row.kind, RowKind::Layer | RowKind::Group { .. } | RowKind::Prop { .. }) {
                out.push(Row {
                    layer,
                    depth: path.len(),
                    kind: RowKind::Inline(Box::new(InlineRow { root, path: path.to_vec(), context: context.clone(), row: row.clone() })),
                });
            }
            i += 1;
        }
        if path.len() < inline_comp::MAX_DEPTH {
            let mut next = path.to_vec();
            next.push(layer.0);
            append(app, root, &next, out);
        }
    }
}

fn command(n: &InlineRow, id: &str, params: serde_json::Value) -> (String, serde_json::Value) {
    ("comp.inlineEdit".into(), json!({"root": n.root.0, "path": n.path, "command": id, "params": params}))
}

fn toggle(set: &mut std::collections::BTreeSet<u64>, id: u64) {
    if !set.remove(&id) {
        set.insert(id);
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn show(
    app: &mut EffectcraftApp,
    ui: &mut egui::Ui,
    n: &InlineRow,
    r: Rect,
    cw: &Cols,
    tm: TMap,
    graph_on: bool,
    left_clip: Rect,
    right_clip: Rect,
    actions: &mut Vec<(String, serde_json::Value)>,
) {
    let Some(comp) = app.session.project.comp_arc(n.context.comp) else { return };
    let Some(layer) = super::super::layer_arc(&comp, n.row.layer) else { return };
    if !crate::widgets::any_menu_open(ui.ctx())
        && !egui::Popup::is_any_open(ui.ctx())
        && ui.input(|i| i.pointer.any_pressed() && i.pointer.interact_pos().is_some_and(|p| r.intersect(left_clip.union(right_clip)).contains(p)))
    {
        focus(app, n.root, &n.path);
    }
    let t = app.tokens;
    let sel = app.session.state.comp_selections.get(&n.context.comp).cloned().unwrap_or_default();
    let selected = sel.layers.contains(&layer.id);
    let locked = layer.switches.locked || n.context.locked;
    let cy = r.center().y;
    let indent = cw.name + 8.0 + 14.0 * (n.path.len() + n.row.depth) as f32;
    let prefix = format!("timeline.inline.{}.layer.{}", instance_key(n.root, &n.path).iter().map(u64::to_string).collect::<Vec<_>>().join("."), layer.id.0);
    let lp = ui.painter().with_clip_rect(left_clip);
    let gp = ui.painter().with_clip_rect(right_clip);
    lp.rect_filled(r, 0.0, if selected { t.row_selected } else { t.row_alt });
    gp.rect_filled(r, 0.0, t.tl_bg);
    lp.line_segment(
        [pos2(cw.name + 14.0 * n.path.len() as f32, r.min.y), pos2(cw.name + 14.0 * n.path.len() as f32, r.max.y)],
        Stroke::new(1.0, t.accent.gamma_multiply(0.5)),
    );
    let id = egui::Id::new((
        &prefix,
        match n.row.kind {
            RowKind::Prop { uid } | RowKind::Group { uid, .. } => uid,
            _ => 0,
        },
    ));
    let mut local = Vec::new();
    let row_auto = match n.row.kind {
        RowKind::Prop { uid } => format!("{prefix}.prop.{uid}.row"),
        RowKind::Group { uid, .. } => format!("{prefix}.group.{uid}.row"),
        _ => format!("{prefix}.row"),
    };
    app.auto.add(&row_auto, r.intersect(left_clip), &layer.name);
    match &n.row.kind {
        RowKind::Layer => {
            for (i, (icon, name, on)) in [
                (Icon::Eye, "video", layer.switches.video),
                (Icon::Speaker, "audio", layer.switches.audio),
                (Icon::Solo, "solo", layer.switches.solo),
                (Icon::Lock, "lock", layer.switches.locked),
            ]
            .into_iter()
            .enumerate()
            {
                let br = Rect::from_center_size(pos2(cw.av + 10.0 + i as f32 * 18.0, cy), vec2(15.0, 15.0));
                if !Vis::of(&app.ui.timeline).av {
                    continue;
                }
                icons::paint(&lp, br, icon, if on { t.text } else { t.text_faint });
                app.auto.add(&format!("{prefix}.{name}"), br, name);
                if ui.interact(br, id.with(name), Sense::click()).clicked() && (!locked || name == "lock") && !n.context.locked {
                    local.push(("layer.setSwitch".into(), json!({"layers":[layer.id.0],"switch":name,"value":!on})));
                }
            }
            let tw = Rect::from_center_size(pos2(indent, cy), vec2(14.0, 14.0));
            if widgets::twirl(ui, tw, app.ui.timeline.open_layers.contains(&layer.id.0), id.with("props"), &t).clicked() {
                toggle(&mut app.ui.timeline.open_layers, layer.id.0);
            }
            app.auto.add(&format!("{prefix}.twirl"), tw, "Layer properties");
            expander(app, ui, n.root, &n.path, &layer, Rect::from_center_size(pos2(indent + 16.0, cy), vec2(14.0, 14.0)));
            let nr = Rect::from_min_max(pos2(indent + 28.0, r.min.y), pos2(cw.name_end - 4.0, r.max.y));
            let response = ui.interact(nr, id.with("name"), Sense::click()).on_hover_text(format!(
                "{} • shared source layers; edits affect every instance",
                app.session.project.item(n.context.comp).map(|item| item.name.as_str()).unwrap_or("Source")
            ));
            widgets::text_fit(&lp, pos2(nr.min.x, cy), Align2::LEFT_CENTER, &layer.name, Tokens::ui(12.0), nr.width(), t.text);
            app.auto.add(&format!("{prefix}.name"), nr, &layer.name);
            if response.clicked() && !locked {
                local.push(("layer.select".into(), json!({"layers":[layer.id.0],"toggle":ui.input(|i|i.modifiers.command || i.modifiers.shift)})));
            }
            response.context_menu(|ui| {
                ui.label("Shared source layer");
                if ui.button("Open source composition").clicked() {
                    local.push(("comp.open".into(), json!({"comp":n.context.comp.0})));
                    ui.close();
                }
                let mut name = layer.name.clone();
                if ui.add_enabled(!locked, egui::TextEdit::singleline(&mut name)).changed() {
                    local.push(("layer.rename".into(), json!({"layer":layer.id.0,"name":name})));
                }
                for (label, direction) in [("Move up", "forward"), ("Move down", "backward")] {
                    if ui.add_enabled(!locked, egui::Button::new(label)).clicked() {
                        local.push(("layer.arrange".into(), json!({"layers":[layer.id.0],"to":direction})));
                        ui.close();
                    }
                }
                for (label, switch, on) in [
                    ("Effects", "fx", layer.switches.effects),
                    ("Motion blur", "motionBlur", layer.switches.motion_blur),
                    ("3D", "threeD", layer.is_3d()),
                    ("Shy", "shy", layer.switches.shy),
                    ("Collapse transformations", "collapse", layer.switches.collapse),
                ] {
                    let mut value = on;
                    if ui.add_enabled(!locked, egui::Checkbox::new(&mut value, label)).changed() {
                        local.push(("layer.setSwitch".into(), json!({"layers":[layer.id.0],"switch":switch,"value":value})));
                    }
                }
            });
            if !graph_on {
                if let Some((offset, scale)) = n.context.affine {
                    let a = tm.x(offset + scale * layer.in_point.seconds());
                    let b = tm.x(offset + scale * layer.out_point.seconds());
                    let bar = Rect::from_min_max(pos2(a.min(b), r.min.y + 3.0), pos2(a.max(b), r.max.y - 3.0)).intersect(right_clip);
                    gp.rect_filled(bar, 2.0, bar_color(t.label(layer.label), selected));
                    ui.set_clip_rect(right_clip);
                    let resp = ui.interact(bar, id.with("bar"), Sense::click_and_drag());
                    app.auto.add(&format!("{prefix}.bar"), bar, "Move nested layer");
                    if resp.drag_started() {
                        ui.ctx().data_mut(|d| d.insert_temp(id.with("bar-origin"), layer.start_time.seconds()));
                    }
                    if resp.dragged() && !locked {
                        let origin = ui.ctx().data(|d| d.get_temp::<f64>(id.with("bar-origin"))).unwrap_or(layer.start_time.seconds());
                        let start = origin + resp.drag_delta().x as f64 / tm.pps / scale;
                        if start.is_finite() {
                            local.push(("layer.timing".into(), json!({"layers":[layer.id.0],"start":start,"merge":format!("bar-{}",layer.id.0)})));
                        }
                    }
                    if resp.drag_stopped() {
                        actions.push(("__endMerge".into(), json!({})));
                    }
                    ui.set_clip_rect(left_clip);
                } else {
                    gp.text(
                        pos2(tm.x0 + 8.0, cy),
                        Align2::LEFT_CENTER,
                        format!("Source {:.3}s • remapped timing", n.context.time.seconds()),
                        Tokens::ui(11.0),
                        t.text_dim,
                    );
                }
            }
        }
        RowKind::Group { uid, name, open, has_children, .. } => {
            let tw = Rect::from_center_size(pos2(indent, cy), vec2(14.0, 14.0));
            if *has_children && widgets::twirl(ui, tw, *open, id.with("group"), &t).clicked() {
                toggle(&mut app.ui.timeline.open_groups, *uid);
            }
            app.auto.add(&format!("{prefix}.group.{uid}.twirl"), tw, "Expand property group");
            lp.text(pos2(indent + 12.0, cy), Align2::LEFT_CENTER, name, Tokens::ui(12.0), t.text);
            if let Some(g) = layer.props.find_group(*uid).filter(|g| g.is_instance()) {
                let eye = Rect::from_center_size(pos2(cw.av + 10.0, cy), vec2(15.0, 15.0));
                icons::paint(&lp, eye, Icon::Eye, if g.enabled { t.text } else { t.text_faint });
                app.auto.add(&format!("{prefix}.group.{uid}.eye"), eye, "Enable group");
                if ui.interact(eye, id.with("eye"), Sense::click()).clicked() && !locked {
                    local.push(("prop.setGroupEnabled".into(), json!({"layer":layer.id.0,"prop":uid,"value":!g.enabled})));
                }
            }
        }
        RowKind::Prop { uid } => {
            let Some(prop) = layer.props.find(*uid) else { return };
            let project = app.session.project.clone();
            let expr = app.session.expr.clone();
            let mut eval = EvalCtx::new(&project, n.context.comp, &comp, n.context.time);
            eval.expr = expr.as_deref();
            let value = eval.value(&layer, prop);
            let sw = Rect::from_center_size(pos2(indent, cy), vec2(15.0, 15.0));
            if !prop.static_only {
                icons::paint(&lp, sw, Icon::Stopwatch, if prop.is_animated() { t.hot_text } else { t.text_dim });
                app.auto.add(&format!("{prefix}.prop.{uid}.stopwatch"), sw, "Toggle animation at nested source time");
                if ui.interact(sw, id.with("stopwatch"), Sense::click()).clicked() && !locked {
                    local.push(("prop.toggleAnimation".into(), json!({"layer":layer.id.0,"prop":uid})));
                }
                let key = Rect::from_center_size(pos2(cw.av + 30.0, cy), vec2(14.0, 14.0));
                let at_key = effectcraft_engine::keyframe::key_at(&prop.keys, layer.layer_time(n.context.time)).is_some();
                icons::paint(&lp, key, Icon::Keyframe, if at_key { t.hot_text } else { t.text_dim });
                app.auto.add(&format!("{prefix}.prop.{uid}.addKey"), key, "Add/remove source keyframe");
                if ui.interact(key, id.with("add-key"), Sense::click()).clicked() && !locked {
                    local.push(("prop.toggleKey".into(), json!({"layer":layer.id.0,"prop":uid})));
                }
            }
            let nr = Rect::from_min_max(pos2(indent + 12.0, r.min.y), pos2((cw.vx - 4.0).max(indent + 110.0), r.max.y));
            widgets::text_fit(
                &lp,
                pos2(nr.min.x, cy),
                Align2::LEFT_CENTER,
                &prop.name,
                Tokens::ui(12.0),
                nr.width(),
                if sel.props.contains(&(layer.id, *uid)) { t.hot_text } else { t.text },
            );
            let name_response = ui.interact(nr, id.with("name"), Sense::click());
            app.auto.add(&format!("{prefix}.prop.{uid}.name"), nr, &prop.name);
            if name_response.clicked() && !locked {
                local.push(("prop.select".into(), json!({"layer":layer.id.0,"prop":uid,"add":ui.input(|i|i.modifiers.shift)})));
            }
            name_response.context_menu(|ui| {
                if ui.button("Open source composition").clicked() {
                    local.push(("comp.open".into(), json!({"comp":n.context.comp.0})));
                    ui.close();
                }
                if !prop.static_only {
                    ui.label("Expression");
                    let buffer = id.with("expression-buffer");
                    let mut text =
                        ui.ctx().data(|d| d.get_temp::<String>(buffer)).unwrap_or_else(|| prop.expr.as_ref().map(|e| e.text.clone()).unwrap_or_default());
                    let response = ui.add_enabled(!locked, egui::TextEdit::multiline(&mut text).desired_width(320.0).desired_rows(3));
                    app.auto.add(&format!("{prefix}.prop.{uid}.expression"), response.rect, "Expression");
                    if response.changed() {
                        ui.ctx().data_mut(|d| d.insert_temp(buffer, text.clone()));
                    }
                    let apply = ui.add_enabled(!locked, egui::Button::new("Apply expression"));
                    app.auto.add(&format!("{prefix}.prop.{uid}.applyExpression"), apply.rect, "Apply expression");
                    if apply.clicked() {
                        local.push(("prop.setExpression".into(), json!({"layer":layer.id.0,"prop":uid,"expression":text})));
                        ui.ctx().data_mut(|d| d.remove::<String>(buffer));
                        ui.close();
                    }
                }
            });
            let at = pos2((cw.vx + 4.0).max(nr.max.x + 5.0), cy);
            ui.add_enabled_ui(!locked, |ui| {
                if matches!(value, Value::Text(_) | Value::Str(_)) {
                    let mut text = value.to_json().as_str().unwrap_or_default().to_string();
                    let rr = Rect::from_min_size(pos2(at.x, at.y - 9.0), vec2(160.0, 18.0));
                    let response = ui.put(rr, egui::TextEdit::singleline(&mut text).id(id.with("text")));
                    app.auto.add(&format!("{prefix}.prop.{uid}.value"), response.rect, "Edit source text");
                    if response.changed() {
                        local.push(("prop.set".into(), json!({"layer":layer.id.0,"prop":uid,"value":text,"merge":format!("text-{uid}")})));
                    }
                    if response.lost_focus() {
                        app.session.history.merge_key = None;
                    }
                } else {
                    value_editor(app, ui, &lp, &layer, prop, &value, at, Some(&instance_key(n.root, &n.path)), &mut local);
                }
            });
            if !graph_on {
                draw_keys(app, ui, n, &layer, prop, r, tm, right_clip, &mut local);
            }
        }
        _ => {}
    }
    ui.set_clip_rect(left_clip);
    for (id, mut params) in local {
        if id == "prop.set" {
            params["selected"] = json!(false);
        }
        if id == "comp.open" {
            actions.push((id, params));
            continue;
        }
        if id == "prop.editGradient" {
            params["comp"] = json!(n.context.comp.0);
            let previous = app.session.state.times.insert(n.context.comp, n.context.time);
            let result = super::super::gradient_editor::open(app, &params);
            if let Some(time) = previous {
                app.session.state.times.insert(n.context.comp, time);
            } else {
                app.session.state.times.remove(&n.context.comp);
            }
            if let Err(error) = result {
                app.ui.status = error;
            }
            continue;
        }
        actions.push(command(n, &id, params));
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_keys(
    app: &mut EffectcraftApp,
    ui: &mut egui::Ui,
    n: &InlineRow,
    layer: &Layer,
    prop: &Property,
    r: Rect,
    tm: TMap,
    clip: Rect,
    actions: &mut Vec<(String, serde_json::Value)>,
) {
    let Some((offset, scale)) = n.context.affine else { return };
    ui.set_clip_rect(clip);
    let p = ui.painter().with_clip_rect(clip);
    let t = app.tokens;
    let selected = app.session.state.comp_selections.get(&n.context.comp).map(|s| s.keys.clone()).unwrap_or_default();
    for (index, k) in prop.keys.iter().enumerate() {
        let root_time = offset + scale * layer.comp_time(k.time).seconds();
        let center = pos2(tm.x(root_time), r.center().y);
        if !clip.expand(8.0).contains(center) {
            continue;
        }
        let kr = effectcraft_engine::KeyRef { layer: layer.id, prop: prop.uid, time: k.time };
        let rect = Rect::from_center_size(center, vec2(12.0, 14.0));
        icons::paint(&p, rect, Icon::Keyframe, if selected.contains(&kr) { t.keyframe_selected } else { t.text_dim });
        let id = egui::Id::new(("inline-key", n.root.0, &n.path, prop.uid, index));
        let resp =
            ui.interact(rect, id, Sense::click_and_drag()).on_hover_text(format!("Source {:.3}s; parent {:.3}s", layer.comp_time(k.time).seconds(), root_time));
        let auto = format!(
            "timeline.inline.{}.layer.{}.prop.{}.key.{index}",
            instance_key(n.root, &n.path).iter().map(u64::to_string).collect::<Vec<_>>().join("."),
            layer.id.0,
            prop.uid
        );
        app.auto.add(&auto, rect, "Source keyframe");
        let select = json!({"keys":[{"layer":layer.id.0,"prop":prop.uid,"time":k.time.seconds()}],"selectProperties":true});
        if layer.switches.locked || n.context.locked {
            continue;
        }
        if resp.clicked() || (resp.drag_started() && !selected.contains(&kr)) {
            let mut args = select.clone();
            args["toggle"] = json!(resp.clicked() && ui.input(|i| i.modifiers.shift));
            actions.push(("keys.select".into(), args));
        }
        if resp.double_clicked() {
            app.session.set_time(Tick::from_seconds_f64(root_time));
        }
        if resp.drag_started() {
            ui.ctx().data_mut(|d| d.insert_temp(id.with("key-applied"), 0.0_f64));
        }
        if resp.dragged()
            && let Some(total) = app.session.project.comp(n.context.comp).and_then(|c| snapped_drag(resp.drag_delta().x, tm.pps, scale, c.frame_rate))
        {
            let applied = ui.ctx().data(|d| d.get_temp::<f64>(id.with("key-applied"))).unwrap_or(0.0);
            if total != applied {
                actions.push(("keys.move".into(), json!({"delta":total-applied,"merge":format!("key-{}",prop.uid)})));
                ui.ctx().data_mut(|d| d.insert_temp(id.with("key-applied"), total));
            }
        }
        if resp.drag_stopped() {
            app.session.history.merge_key = None;
        }
        resp.context_menu(|ui| {
            for (label, command) in [("Easy ease", "keys.easyEase"), ("Hold", "keys.toggleHold"), ("Delete keyframe", "keys.delete")] {
                if ui.button(label).clicked() {
                    actions.push(("keys.select".into(), select.clone()));
                    actions.push((command.into(), json!({})));
                    ui.close();
                }
            }
        });
    }
}

fn snapped_drag(pixels: f32, pps: f64, scale: f64, rate: effectcraft_engine::time::FrameRate) -> Option<f64> {
    if !pps.is_finite() || pps <= 0.0 || !scale.is_finite() || scale.abs() < 1e-9 {
        return None;
    }
    let seconds = pixels as f64 / pps / scale;
    seconds.is_finite().then(|| rate.snap_nearest(Tick::from_seconds_f64(seconds)).seconds())
}

fn focus(app: &mut EffectcraftApp, root: ItemId, path: &[u64]) {
    if app.ui.timeline.inline_parent_selection.is_none() {
        let state = &mut app.session.state;
        app.ui.timeline.inline_parent_text_edit = state.text_edit.take();
        app.ui.timeline.inline_parent_selection = Some(effectcraft_engine::CompSelection {
            layers: std::mem::take(&mut state.selected_layers),
            props: std::mem::take(&mut state.selected_props),
            keys: std::mem::take(&mut state.selected_keys),
            vertices: std::mem::take(&mut state.selected_vertices),
        });
    }
    app.ui.timeline.inline_focus = Some(instance_key(root, path));
}

pub(super) fn clear_focus(app: &mut EffectcraftApp) {
    let text_edit = app.ui.timeline.inline_parent_text_edit.take();
    let root = app.ui.timeline.inline_focus.take().and_then(|p| p.first().copied()).map(ItemId);
    let Some(mut selection) = app.ui.timeline.inline_parent_selection.take() else { return };
    let Some(root) = root else { return };
    let Some(comp) = app.session.project.comp(root) else { return };
    let text_edit = text_edit.filter(|edit| comp.layer(edit.layer).is_some_and(|layer| matches!(layer.source, LayerSource::Text)));
    selection.layers.retain(|id| comp.layer(*id).is_some());
    selection.props.retain(|(id, uid)| comp.layer(*id).is_some_and(|l| l.props.find(*uid).is_some() || l.props.find_group(*uid).is_some()));
    selection.keys.retain(|k| comp.layer(k.layer).and_then(|l| l.props.find(k.prop)).is_some_and(|p| p.keys.iter().any(|key| key.time == k.time)));
    selection.vertices.retain(|v| comp.layer(v.layer).and_then(|l| l.props.find_group(v.mask)).is_some());
    if app.session.active_comp_id() == Some(root) {
        let state = &mut app.session.state;
        if state.text_edit.is_none() {
            state.text_edit = text_edit;
        }
        if state.selected_layers.is_empty() && state.selected_props.is_empty() && state.selected_keys.is_empty() && state.selected_vertices.is_empty() {
            state.selected_layers = selection.layers;
            state.selected_props = selection.props;
            state.selected_keys = selection.keys;
            state.selected_vertices = selection.vertices;
        }
    } else {
        app.session.state.comp_selections.insert(root, selection);
    }
}

/// Parent selection is parked while inline focus owns edits; unsupported actions never fall through.
pub(super) fn target(app: &mut EffectcraftApp, id: &str, params: &serde_json::Value) -> Result<Option<serde_json::Value>, String> {
    let Some(focus) = app.ui.timeline.inline_focus.clone() else { return Ok(None) };
    if app.ui.focused != crate::dock::PanelKind::Timeline || focus.first().copied() != app.session.active_comp_id().map(|c| c.0) {
        clear_focus(app);
        return Ok(None);
    }
    if id == "comp.inlineEdit" || ["comp", "layer", "layers", "prop", "keys"].iter().any(|key| params.get(key).is_some()) {
        return Ok(None);
    }
    let Some((&root, path)) = focus.split_first() else { return Ok(None) };
    let selection_command = matches!(
        id,
        "edit.clear"
            | "edit.duplicate"
            | "keys.delete"
            | "keys.easyEase"
            | "keys.easyEaseIn"
            | "keys.easyEaseOut"
            | "keys.toggleHold"
            | "prop.reset"
            | "layer.setSwitch"
    );
    if selection_command {
        return Ok(Some(json!({"root":root,"path":path,"command":id,"params":params})));
    }
    if matches!(id, "edit.cut" | "edit.copy" | "edit.paste" | "edit.selectAll" | "edit.deselectAll" | "timeline.graphEditor")
        || ["layer.", "prop.", "keys.", "mask.", "shape.", "anim.", "timeline.reveal", "timeline.trim", "timeline.moveIn", "timeline.moveOut", "viewer.nudge"]
            .iter()
            .any(|prefix| id.starts_with(prefix))
    {
        return Err("This action needs the source composition open. Use the nested layer's context menu; the parent was not changed.".into());
    }
    Ok(None)
}

pub(super) fn enabled(app: &EffectcraftApp, id: &str) -> Option<bool> {
    if app.ui.focused != crate::dock::PanelKind::Timeline {
        return None;
    }
    let focus = app.ui.timeline.inline_focus.as_ref()?;
    let (&root, path) = focus.split_first()?;
    if Some(root) != app.session.active_comp_id().map(|c| c.0) {
        return None;
    }
    let context = inline_comp::context(&app.session, ItemId(root), path, app.session.time_of(ItemId(root))).ok()?;
    let selection = app.session.state.comp_selections.get(&context.comp).cloned().unwrap_or_default();
    let selected = match id {
        "edit.clear" => !selection.layers.is_empty() || !selection.props.is_empty() || !selection.keys.is_empty() || !selection.vertices.is_empty(),
        "edit.duplicate" | "layer.setSwitch" => !selection.layers.is_empty(),
        "keys.delete" | "keys.easyEase" | "keys.easyEaseIn" | "keys.easyEaseOut" | "keys.toggleHold" => !selection.keys.is_empty(),
        "prop.reset" => !selection.props.is_empty(),
        "edit.cut" | "edit.copy" | "edit.paste" | "edit.selectAll" | "edit.deselectAll" => false,
        _ => return None,
    };
    Some(selected && !context.locked)
}

#[cfg(test)]
mod tests {
    use super::*;
    use effectcraft_engine::Session;

    fn fixture() -> (EffectcraftApp, ItemId, ItemId, u64, u64, u64) {
        let mut s = Session::default();
        let child = ItemId(s.execute("comp.new", json!({"name":"Source"})).unwrap()["comp"].as_u64().unwrap());
        let leaf = s.execute("layer.newSolid", json!({"name":"Shared layer"})).unwrap()["layer"].as_u64().unwrap();
        let root = ItemId(s.execute("comp.new", json!({"name":"Parent"})).unwrap()["comp"].as_u64().unwrap());
        let a = s.execute("layer.addItem", json!({"item":child.0})).unwrap()["layer"].as_u64().unwrap();
        let b = s.execute("layer.addItem", json!({"item":child.0})).unwrap()["layer"].as_u64().unwrap();
        (EffectcraftApp::new(s), root, child, leaf, a, b)
    }

    #[test]
    fn slow_key_drag_accumulates_before_snapping_including_reverse_stretch() {
        let rate = effectcraft_engine::time::FrameRate::FPS_30;
        for scale in [2.0, -2.0] {
            let mut applied = 0.0;
            let mut steps = 0.0;
            for i in 1..=40 {
                let total = snapped_drag(i as f32 * 0.1, 60.0, scale, rate).unwrap();
                steps += total - applied;
                applied = total;
            }
            assert!((steps - scale.signum() / 30.0).abs() < 1e-9);
        }
        assert_eq!(snapped_drag(1.0, 60.0, 0.0, rate), None);
    }

    #[test]
    fn leaving_inline_focus_keeps_a_new_viewer_selection_and_enables_source_menus() {
        let (mut app, root, child, leaf, a, b) = fixture();
        app.ui.focused = crate::dock::PanelKind::Timeline;
        app.session.execute("layer.select", json!({"layers":[a]})).unwrap();
        focus(&mut app, root, &[a]);
        app.session.execute("comp.inlineEdit", json!({"path":[a],"command":"layer.select","params":{"layers":[leaf]}})).unwrap();
        assert_eq!(enabled(&app, "edit.clear"), Some(true));
        assert_eq!(enabled(&app, "edit.duplicate"), Some(true));
        assert_eq!(enabled(&app, "keys.easyEase"), Some(false));
        assert_eq!(app.session.state.comp_selections[&child].layers, vec![LayerId(leaf)]);
        app.session.execute("layer.select", json!({"layers":[b]})).unwrap();
        clear_focus(&mut app);
        assert_eq!(app.session.state.selected_layers, vec![LayerId(b)]);
    }

    #[test]
    fn inline_focus_hides_parent_inspector_selection_and_restores_on_exit() {
        let (mut app, root, _, _, a, b) = fixture();
        app.session.execute("layer.select", json!({"layers":[b]})).unwrap();
        focus(&mut app, root, &[a]);
        assert!(app.session.state.selected_layers.is_empty());
        assert!(app.session.state.selected_props.is_empty());
        clear_focus(&mut app);
        assert_eq!(app.session.state.selected_layers, vec![LayerId(b)]);
        assert!(app.ui.timeline.inline_focus.is_none());
    }

    #[test]
    fn inline_delete_routes_to_source_and_unsupported_shortcuts_never_touch_parent() {
        let (mut app, root, child, leaf, a, _) = fixture();
        app.ui.focused = crate::dock::PanelKind::Timeline;
        app.ui.timeline.inline_focus = Some(instance_key(root, &[a]));
        app.session.execute("comp.inlineEdit", json!({"path":[a],"command":"layer.select","params":{"layers":[leaf]}})).unwrap();
        let ctx = egui::Context::default();
        assert!(crate::menus::invoke(&mut app, &ctx, "edit.cut", json!({})).is_err());
        assert_eq!(app.session.project.comp(root).unwrap().layers.len(), 2);
        crate::menus::invoke(&mut app, &ctx, "edit.clear", json!({})).unwrap();
        assert_eq!(app.session.project.comp(root).unwrap().layers.len(), 2);
        assert!(app.session.project.comp(child).unwrap().layers.is_empty());
        assert_eq!(app.session.active_comp_id(), Some(root));
    }

    #[test]
    fn repeated_instances_have_separate_expansion_and_shared_layer_identity() {
        let (mut app, root, child, leaf, a, b) = fixture();
        app.ui.timeline.inline_precomps.insert(instance_key(root, &[a]));
        let rows = expand(&app, root, build_rows(&app, app.session.project.comp(root).unwrap()));
        let projected: Vec<_> = rows
            .iter()
            .filter_map(|r| match &r.kind {
                RowKind::Inline(n) => Some(n),
                _ => None,
            })
            .collect();
        assert_eq!(projected.len(), 1);
        assert_eq!(projected[0].path, vec![a]);
        assert_eq!(projected[0].context.comp, child);
        assert_eq!(projected[0].row.layer, LayerId(leaf));
        app.ui.timeline.inline_precomps.insert(instance_key(root, &[b]));
        let rows = expand(&app, root, build_rows(&app, app.session.project.comp(root).unwrap()));
        let projected: Vec<_> = rows
            .iter()
            .filter_map(|r| match &r.kind {
                RowKind::Inline(n) => Some(n),
                _ => None,
            })
            .collect();
        assert_eq!(projected.len(), 2);
        assert_eq!(projected[0].row.layer, projected[1].row.layer);
        assert_ne!(projected[0].path, projected[1].path);
        assert_eq!(app.session.project.comp(child).unwrap().layers.len(), 1);
    }

    #[test]
    fn nested_property_rows_are_not_copied_or_bound_to_parent_comp() {
        let (mut app, root, child, leaf, a, _) = fixture();
        app.ui.timeline.inline_precomps.insert(instance_key(root, &[a]));
        app.ui.timeline.open_layers.insert(leaf);
        let group = app.session.project.comp(child).unwrap().layer(LayerId(leaf)).unwrap().transform().unwrap().uid;
        app.ui.timeline.open_groups.insert(group);
        let rows = expand(&app, root, build_rows(&app, app.session.project.comp(root).unwrap()));
        let nested = rows
            .iter()
            .find_map(|r| match &r.kind {
                RowKind::Inline(n) if matches!(n.row.kind, RowKind::Prop { .. }) => Some(n),
                _ => None,
            })
            .unwrap();
        let (id, args) = command(nested, "prop.set", json!({"layer":leaf,"path":"transform/opacity","value":25}));
        app.session.execute(&id, args).unwrap();
        assert_eq!(app.session.active_comp_id(), Some(root));
        assert_eq!(app.session.project.comp(child).unwrap().layer(LayerId(leaf)).unwrap().props.prop("transform/opacity").unwrap().value.as_f64(), 25.0);
    }
}
