//! Panel bodies.

// Every `src/panels/*.rs` file is a module, declared by build.rs (docs/architecture.md,
// "Self-registering modules"); declare one here only for another visibility or a submodule folder.
include!(concat!(env!("OUT_DIR"), "/panels.rs"));

use effectcraft_engine::Session;
use effectcraft_engine::project::Comp;
use effectcraft_engine::time::Tick;
use egui::Rect;

use crate::EffectcraftApp;
use crate::dock::PanelKind;

/// Drag-and-drop payloads between panels.
#[derive(Clone, Debug)]
pub enum DragPayload {
    /// A project item (footage, comp, solid).
    Item(u64),
    /// An effect id from Effects & Presets.
    Effect(String),
    /// A property dragged from the timeline (to the Essential Graphics panel).
    Property { layer: u64, prop: u64 },
    /// Files from the Media Browser (import; into the timeline: import and add).
    Files(Vec<String>),
}

/// The current time formatted per project settings (timecode with `;` for drop-frame, frames
/// or Feet + Frames).
pub fn timecode(session: &Session, comp: &Comp, t: Tick) -> String {
    effectcraft_engine::commands::time::display_time(session, comp, t)
}

pub fn show(app: &mut EffectcraftApp, ui: &mut egui::Ui, p: PanelKind, rect: Rect) {
    match p {
        PanelKind::Composition => viewers::show(app, ui, 0, rect),
        PanelKind::Viewer(n) => viewers::show(app, ui, n, rect),
        PanelKind::Timeline => timeline::show(app, ui, rect),
        PanelKind::Project => project::show(app, ui, rect),
        PanelKind::EffectControls => effect_controls::show(app, ui, rect),
        PanelKind::EffectsPresets => effects_presets::show(app, ui, rect),
        PanelKind::Info => info::show(app, ui, rect),
        PanelKind::Preview => misc::preview(app, ui, rect),
        PanelKind::Character => text_panels::character(app, ui, rect),
        PanelKind::Paragraph => text_panels::paragraph(app, ui, rect),
        PanelKind::Align => text_panels::align(app, ui, rect),
        PanelKind::Properties => properties::show(app, ui, rect),
        PanelKind::Audio => misc::audio(app, ui, rect),
        PanelKind::History => misc::history(app, ui, rect),
        PanelKind::Markers => misc::markers(app, ui, rect),
        PanelKind::MaskInterpolation => anim_tools::mask_interpolation(app, ui, rect),
        PanelKind::ScriptConsole => script_console::show(app, ui, rect),
        PanelKind::Wiggler => anim_tools::wiggler(app, ui, rect),
        PanelKind::Smoother => anim_tools::smoother(app, ui, rect),
        PanelKind::MotionSketch => anim_tools::motion_sketch(app, ui, rect),
        PanelKind::RenderQueue => render_queue::show(app, ui, rect),
        PanelKind::Tracker => tracker::show(app, ui, rect),
        PanelKind::Layer => layer_panel::show(app, ui, rect),
        PanelKind::Paint => paint_panels::paint(app, ui, rect),
        PanelKind::Brushes => paint_panels::brushes(app, ui, rect),
        PanelKind::EssentialGraphics => essential::show(app, ui, rect),
        PanelKind::ScriptPanel(id) => scriptui_view::panel(app, ui, rect, id),
        PanelKind::Flowchart => flowchart::show(app, ui, rect),
        PanelKind::LumetriScopes => scopes_panel::show(app, ui, rect),
        PanelKind::Footage => footage_panel::show(app, ui, rect),
        PanelKind::MediaBrowser => media_panels::media_browser(app, ui, rect),
        PanelKind::Metadata => media_panels::metadata(app, ui, rect),
        PanelKind::Progress => media_panels::progress(app, ui, rect),
        PanelKind::ContentAwareFill => content_fill_panel::show(app, ui, rect),
        PanelKind::CreateNullsFromPaths => path_vr_panels::create_nulls(app, ui, rect),
        PanelKind::VrCompEditor => path_vr_panels::vr_editor(app, ui, rect),
    }
}

/// The ≡ panel menu (opened from a tab).
pub fn panel_menu_popup(app: &mut EffectcraftApp, ui: &mut egui::Ui) {
    let id = egui::Id::new("panel-menu");
    let Some((panel, pos)) = ui.ctx().data(|d| d.get_temp::<(PanelKind, egui::Pos2)>(id)) else { return };
    let mut close = false;
    let area = egui::Area::new(id.with("area")).order(egui::Order::Foreground).fixed_pos(pos).show(ui.ctx(), |ui| {
        egui::Frame::popup(ui.style()).show(ui, |ui| {
            ui.set_min_width(200.0);
            if ui.button("Close Panel").clicked() {
                app.close_panel(panel);
                close = true;
            }
            let floating = app.ui.floating.iter().any(|f| f.panels.contains(&panel));
            if !floating && ui.button("Undock Panel").clicked() {
                let r = crate::dock_ui::default_float_rect(ui.ctx().content_rect());
                app.edit_layout(|l| l.float(panel, r));
                close = true;
            }
            if !floating && ui.button(if app.ui.maximized == Some(panel) { "Restore Panel Size" } else { "Maximize Panel" }).clicked() {
                app.toggle_maximize(panel);
                close = true;
            }
            ui.separator();
            match panel {
                PanelKind::Timeline => {
                    for (label, id) in [
                        ("Hide Shy Layers", "hideShy"),
                        ("Enable Frame Blending", "frameBlending"),
                        ("Enable Motion Blur", "motionBlur"),
                        ("Draft 3D", "draft3d"),
                    ] {
                        if ui.button(label).clicked() {
                            let _ = app.session.execute("comp.setSwitch", serde_json::json!({"switch": id}));
                            close = true;
                        }
                    }
                    if ui.button("Composition Settings…").clicked() {
                        let _ = dialogs::open_comp_settings(app);
                        close = true;
                    }
                }
                PanelKind::Composition | PanelKind::Viewer(_) => {
                    for (label, cmd) in [
                        ("New Viewer", "view.newViewer"),
                        ("Composition Settings…", "app.compSettings"),
                        ("View Options…", "view.layerControls"),
                        ("Show Grid", "view.grid"),
                        ("Title/Action Safe", "view.safeMargins"),
                    ] {
                        if ui.button(label).clicked() {
                            let _ = crate::menus::invoke(app, &ui.ctx().clone(), cmd, serde_json::json!({}));
                            close = true;
                        }
                    }
                }
                _ => {
                    ui.label(egui::RichText::new(panel.title()).weak());
                }
            }
        });
    });
    if close || (ui.input(|i| i.pointer.any_pressed()) && !area.response.contains_pointer() && !area.response.hovered()) {
        ui.ctx().data_mut(|d| d.remove::<(PanelKind, egui::Pos2)>(id));
    }
}
