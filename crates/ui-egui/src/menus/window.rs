//! Window menu and docking: panels, workspaces, the panel menu, docking / undocking / closing
//! panels and panel groups.

use serde_json::{Value, json};

use super::dispatch::Handler;
use crate::EffectcraftApp;
use crate::dock::PanelKind;

pub(crate) fn handlers() -> &'static [Handler] {
    const H: &[Handler] = &[
        // Legacy per-panel / per-workspace ids (`window.panel.Project`, `window.workspace.default`).
        Handler::ui_prefix("window.panel.", legacy_panel),
        Handler::ui_prefix("window.workspace.", legacy_workspace),
        Handler::ui("window.maximizePanel", panel_op),
        Handler::ui("window.dockPanel", panel_op),
        Handler::ui("window.floatPanel", panel_op),
        Handler::ui("window.closePanel", panel_op),
        Handler::ui("window.closeOtherPanels", panel_op),
        Handler::ui("window.closePanelGroup", panel_op),
        Handler::ui("window.floatPanelGroup", panel_op),
        Handler::ui("window.stackPanelGroup", panel_op),
        Handler::ui("window.panelMenu", panel_op),
        // Window ▸ <ScriptUI panel>: dock (or bring forward) the panel the script built.
        Handler::frontend("window.scriptPanel", script_panel),
        Handler::frontend("window.panel", panel),
        Handler::frontend("window.workspace", workspace),
        Handler::frontend("window.resetWorkspace", reset_workspace),
        Handler::frontend("window.saveWorkspace", save_workspace),
        Handler::frontend("window.saveWorkspaceAs", save_workspace_as),
        Handler::frontend("window.editWorkspaces", edit_workspaces),
        Handler::frontend("window.assignWorkspaceShortcut", assign_workspace_shortcut),
    ];
    H
}

fn legacy_panel(app: &mut EffectcraftApp, ctx: &egui::Context, id: &str, _params: Value) -> Result<Value, String> {
    let rest = id.strip_prefix("window.panel.").unwrap_or(id);
    super::frontend(app, ctx, "window.panel", json!({"panel": rest}))
}

fn legacy_workspace(app: &mut EffectcraftApp, ctx: &egui::Context, id: &str, _params: Value) -> Result<Value, String> {
    let ws = id.strip_prefix("window.workspace.").unwrap_or(id);
    if ws == "reset" {
        return super::frontend(app, ctx, "window.resetWorkspace", json!({}));
    }
    super::frontend(app, ctx, "window.workspace", json!({"name": ws}))
}

/// Docking and the panel menu: {panel, anchor?, zone: center|left|right|top|bottom},
/// {panel, rect?}, {panel?}, {panel, comp?}, {panel, stacked?}.
fn panel_op(app: &mut EffectcraftApp, ctx: &egui::Context, id: &str, params: Value) -> Result<Value, String> {
    let op = id.strip_prefix("window.").unwrap_or(id);
    let panel_of = |k: &str| params.get(k).and_then(Value::as_str).map(|s| PanelKind::from_name(s).ok_or(format!("unknown panel `{s}`")));
    let need = |op: &str| -> Result<PanelKind, String> { panel_of("panel").ok_or(format!("{op}: need `panel`"))? };
    // A Timeline comp tab: `comp` (its id).
    let comp = params.get("comp").and_then(Value::as_u64);
    let rect = params.get("rect").and_then(Value::as_array).map(|a| {
        let g = |i: usize| a.get(i).and_then(Value::as_f64).unwrap_or(0.0) as f32;
        [g(0), g(1), g(2), g(3)]
    });
    match op {
        "closeOtherPanels" => {
            let n = app.close_other_panels(ctx, need(op)?, comp)?;
            Ok(json!({"closed": n}))
        }
        "closePanelGroup" => {
            let n = app.close_panel_group(need(op)?)?;
            Ok(json!({"closed": n}))
        }
        "floatPanelGroup" => {
            let p = need(op)?;
            let r = rect.unwrap_or_else(|| crate::dock_ui::default_float_rect(ctx.content_rect()));
            if !app.edit_layout(|l| l.float_group(p, r)) {
                return Err(format!("can't undock the panel group of {}", p.title()));
            }
            Ok(Value::Null)
        }
        "stackPanelGroup" => {
            let on = app.stack_panel_group(need(op)?, params.get("stacked").and_then(Value::as_bool))?;
            Ok(json!({"stacked": on}))
        }
        "panelMenu" => {
            let p = need(op)?;
            app.panel_group(p).ok_or_else(|| format!("{} is not open", p.title()))?;
            let at = |k: &str| params.get(k).and_then(Value::as_f64).map(|v| v as f32);
            let tab = app.auto.find(&format!("panel.tab.{}", p.id())).map(|e| egui::pos2(e.rect[0], e.rect[1] + e.rect[3]));
            let pos = match (at("x"), at("y")) {
                (Some(x), Some(y)) => egui::pos2(x, y),
                _ => tab.unwrap_or_else(|| ctx.content_rect().center()),
            };
            let doc = comp.or_else(|| app.shown_doc(p));
            crate::panels::panel_menu::open(ctx, p, doc, pos);
            Ok(Value::Null)
        }
        "maximizePanel" => {
            let p = match panel_of("panel") {
                Some(p) => p?,
                None => app.panel_at(ctx.pointer_hover_pos()),
            };
            app.toggle_maximize(p);
            Ok(json!({"maximized": app.ui.maximized}))
        }
        "closePanel" => {
            let p = need(op)?;
            app.close_panel_tab(ctx, p, comp)?;
            Ok(Value::Null)
        }
        "floatPanel" => {
            let p = need(op)?;
            let r = rect.unwrap_or_else(|| crate::dock_ui::default_float_rect(ctx.content_rect()));
            if !app.edit_layout(|l| l.float(p, r)) {
                return Err(format!("can't undock {}", p.title()));
            }
            Ok(Value::Null)
        }
        _ => {
            let p = need(op)?;
            // Without an anchor: back to its usual docked place.
            let Some(anchor) = panel_of("anchor") else {
                app.redock_panel(p);
                return Ok(Value::Null);
            };
            let anchor = anchor?;
            let zone = params
                .get("zone")
                .and_then(Value::as_str)
                .map_or(Some(crate::dock::Zone::Center), crate::dock::Zone::from_name)
                .ok_or("zone: center|left|right|top|bottom")?;
            if !app.edit_layout(|l| l.dock(p, anchor, zone)) {
                return Err(format!("can't dock {} at {} ({zone:?})", p.title(), anchor.title()));
            }
            app.show_panel(p);
            Ok(Value::Null)
        }
    }
}

fn script_panel(app: &mut EffectcraftApp, _ctx: &egui::Context, _id: &str, p: Value) -> Result<Value, String> {
    let id = p.get("window").and_then(Value::as_u64).ok_or("no ScriptUI panel window")? as u32;
    app.show_panel(PanelKind::ScriptPanel(id));
    Ok(json!({"window": id}))
}

fn panel(app: &mut EffectcraftApp, _ctx: &egui::Context, _id: &str, p: Value) -> Result<Value, String> {
    let name = p.get("panel").and_then(Value::as_str).unwrap_or_default();
    if name.eq_ignore_ascii_case("tools") {
        app.ui.status = "The Tools panel is the toolbar in the header".into();
        return Ok(Value::Null);
    }
    let panel = PanelKind::from_name(name).ok_or_else(|| format!("unknown panel `{name}`"))?;
    app.show_panel(panel);
    Ok(Value::Null)
}

fn workspace(app: &mut EffectcraftApp, _ctx: &egui::Context, _id: &str, p: Value) -> Result<Value, String> {
    let want = p.get("name").and_then(Value::as_str).unwrap_or("Default").to_ascii_lowercase().replace(' ', "");
    let name =
        app.workspace_names().into_iter().find(|w| w.to_ascii_lowercase().replace(' ', "") == want).ok_or_else(|| format!("unknown workspace `{want}`"))?;
    app.set_workspace(&name);
    Ok(json!({"workspace": name}))
}

fn reset_workspace(app: &mut EffectcraftApp, _ctx: &egui::Context, _id: &str, _p: Value) -> Result<Value, String> {
    let name = app.ui.workspace.clone();
    app.set_workspace(&name);
    Ok(Value::Null)
}

fn save_workspace(app: &mut EffectcraftApp, _ctx: &egui::Context, _id: &str, _p: Value) -> Result<Value, String> {
    let name = app.ui.workspace.clone();
    app.ui.saved_workspaces.insert(name.clone(), app.ui.dock.clone());
    app.ui.saved_floating.insert(name.clone(), app.ui.floating.clone());
    Ok(json!({"workspace": name}))
}

fn save_workspace_as(app: &mut EffectcraftApp, _ctx: &egui::Context, _id: &str, p: Value) -> Result<Value, String> {
    let Some(name) = p.get("name").and_then(Value::as_str).map(str::trim).filter(|n| !n.is_empty()).map(str::to_string) else {
        crate::panels::dialogs::form(
            app,
            "New Workspace",
            "window.saveWorkspaceAs",
            json!({}),
            vec![crate::panels::dialogs::Field::text("name", "Name", "Untitled Workspace")],
        );
        return Ok(Value::Null);
    };
    app.ui.saved_workspaces.insert(name.clone(), app.ui.dock.clone());
    app.ui.saved_floating.insert(name.clone(), app.ui.floating.clone());
    app.ui.workspace = name.clone();
    Ok(json!({"workspace": name}))
}

fn edit_workspaces(app: &mut EffectcraftApp, _ctx: &egui::Context, _id: &str, p: Value) -> Result<Value, String> {
    let Some(name) = p.get("name").and_then(Value::as_str).map(str::to_string) else {
        let cur = app.ui.workspace.clone();
        crate::panels::dialogs::form(
            app,
            "Edit Workspaces",
            "window.editWorkspaces",
            json!({}),
            vec![
                crate::panels::dialogs::Field::text("name", "Workspace", &cur),
                crate::panels::dialogs::Field::text("rename", "Rename to", &cur),
                crate::panels::dialogs::Field::bool("delete", "Delete", false),
            ],
        );
        return Ok(Value::Null);
    };
    let builtin = crate::dock::WORKSPACES.contains(&name.as_str());
    if p.get("delete").and_then(Value::as_bool) == Some(true) {
        app.ui.saved_floating.remove(&name);
        if app.ui.saved_workspaces.remove(&name).is_none() && !builtin {
            return Err(format!("no workspace `{name}`"));
        }
        if app.ui.workspace == name {
            app.set_workspace("Default");
        }
        return Ok(json!({"deleted": name}));
    }
    if let Some(new) = p.get("rename").and_then(Value::as_str).map(str::trim).filter(|n| !n.is_empty() && *n != name) {
        if builtin {
            return Err(format!("`{name}` is a built-in workspace: save it as a new workspace instead"));
        }
        let dock = app.ui.saved_workspaces.remove(&name).ok_or_else(|| format!("no workspace `{name}`"))?;
        app.ui.saved_workspaces.insert(new.to_string(), dock);
        if let Some(f) = app.ui.saved_floating.remove(&name) {
            app.ui.saved_floating.insert(new.to_string(), f);
        }
        if app.ui.workspace == name {
            app.ui.workspace = new.to_string();
        }
        return Ok(json!({"renamed": new}));
    }
    Ok(json!({"workspaces": app.workspace_names()}))
}

fn assign_workspace_shortcut(app: &mut EffectcraftApp, ctx: &egui::Context, _id: &str, p: Value) -> Result<Value, String> {
    let mut q = p.clone();
    q["workspace"] = json!(app.ui.workspace);
    super::run_engine(app, ctx, "window.assignWorkspaceShortcut", q)
}
