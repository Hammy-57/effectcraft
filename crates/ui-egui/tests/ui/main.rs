//! One test binary for the effectcraft-ui-egui integration tests: every `tests/*.rs` file is a
//! module here, so the app is linked once instead of once per file (`autotests = false`).
//!
//! - Run one file's tests: `cargo test -p effectcraft-ui-egui --test ui ui_viewer::` (a name filter).
//! - A new `tests/x.rs` needs its two lines below (`#[path = "../x.rs"] mod x;`); cargo ignores it
//!   otherwise, so `every_test_file_is_a_module` fails until it has them.
//! - Tests of different files share one process and run in parallel.
//!   A test that creates a GPU device holds [`gpu_lock`]. `-- --ignored` runs the opt-in snapshots.

/// One GPU device at a time in this binary: wgpu's OpenGL backend (Mesa llvmpipe on FreeBSD,
/// Linux without Vulkan) panics or crashes when tests create and drive devices concurrently.
/// Tests that run by default and create a device (`Gpu::headless*`, a `.wgpu()` harness,
/// `Harness::render`) hold this guard.
pub fn gpu_lock() -> std::sync::MutexGuard<'static, ()> {
    static GPU: std::sync::Mutex<()> = std::sync::Mutex::new(());
    GPU.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[test]
fn every_test_file_is_a_module() {
    let main = include_str!("main.rs");
    let missing: Vec<String> = std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/tests"))
        .unwrap()
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|f| f.ends_with(".rs") && !main.contains(&format!("#[path = \"../{f}\"]")))
        .collect();
    assert!(missing.is_empty(), "not compiled: declare each in tests/ui/main.rs (`#[path = \"../x.rs\"] mod x;`): {missing:?}");
}

#[path = "../render_queue_snapshot.rs"]
mod render_queue_snapshot;
#[path = "../ui_3d.rs"]
mod ui_3d;
#[path = "../ui_agent_audit.rs"]
mod ui_agent_audit;
#[path = "../ui_angles.rs"]
mod ui_angles;
#[path = "../ui_audio_scrub.rs"]
mod ui_audio_scrub;
#[path = "../ui_azerty_shortcuts.rs"]
mod ui_azerty_shortcuts;
#[path = "../ui_blank_context_menus.rs"]
mod ui_blank_context_menus;
#[path = "../ui_camera_tracker.rs"]
mod ui_camera_tracker;
#[path = "../ui_color_hex.rs"]
mod ui_color_hex;
#[path = "../ui_control_channel.rs"]
mod ui_control_channel;
#[path = "../ui_drag_drop.rs"]
mod ui_drag_drop;
#[path = "../ui_effect_controls.rs"]
mod ui_effect_controls;
#[path = "../ui_fingerprints.rs"]
mod ui_fingerprints;
#[path = "../ui_float_windows.rs"]
mod ui_float_windows;
#[path = "../ui_font_search.rs"]
mod ui_font_search;
#[path = "../ui_fx_console.rs"]
mod ui_fx_console;
#[path = "../ui_fx_editors.rs"]
mod ui_fx_editors;
#[path = "../ui_gpu.rs"]
mod ui_gpu;
#[path = "../ui_graph_fit.rs"]
mod ui_graph_fit;
#[path = "../ui_inline_edit.rs"]
mod ui_inline_edit;
#[path = "../ui_keyframes.rs"]
mod ui_keyframes;
#[path = "../ui_large_project.rs"]
mod ui_large_project;
#[path = "../ui_live_preview.rs"]
mod ui_live_preview;
#[path = "../ui_local_assets.rs"]
mod ui_local_assets;
#[path = "../ui_locked_color.rs"]
mod ui_locked_color;
#[path = "../ui_menus.rs"]
mod ui_menus;
#[path = "../ui_panel_focus.rs"]
mod ui_panel_focus;
#[path = "../ui_panel_menu.rs"]
mod ui_panel_menu;
#[path = "../ui_panels.rs"]
mod ui_panels;
#[path = "../ui_parameter_spacing.rs"]
mod ui_parameter_spacing;
#[path = "../ui_preview_align.rs"]
mod ui_preview_align;
#[path = "../ui_project_delete.rs"]
mod ui_project_delete;
#[path = "../ui_project_rows.rs"]
mod ui_project_rows;
#[path = "../ui_properties.rs"]
mod ui_properties;
#[path = "../ui_ram_preview.rs"]
mod ui_ram_preview;
#[path = "../ui_render_queue.rs"]
mod ui_render_queue;
#[path = "../ui_script_console.rs"]
mod ui_script_console;
#[path = "../ui_scriptui.rs"]
mod ui_scriptui;
#[path = "../ui_scrolling.rs"]
mod ui_scrolling;
#[path = "../ui_settings_info.rs"]
mod ui_settings_info;
#[path = "../ui_solo_small_tabs.rs"]
mod ui_solo_small_tabs;
#[path = "../ui_source_icons.rs"]
mod ui_source_icons;
#[path = "../ui_tabs.rs"]
mod ui_tabs;
#[path = "../ui_tail.rs"]
mod ui_tail;
#[path = "../ui_text_edit.rs"]
mod ui_text_edit;
#[path = "../ui_time_zoom_controls.rs"]
mod ui_time_zoom_controls;
#[path = "../ui_timeline_shapes.rs"]
mod ui_timeline_shapes;
#[path = "../ui_timeline_switches.rs"]
mod ui_timeline_switches;
#[path = "../ui_timeline_trim.rs"]
mod ui_timeline_trim;
#[path = "../ui_timeline_zoom.rs"]
mod ui_timeline_zoom;
#[path = "../ui_tracker.rs"]
mod ui_tracker;
#[path = "../ui_viewer.rs"]
mod ui_viewer;
#[path = "../ui_viewers.rs"]
mod ui_viewers;
#[path = "../ui_visual_review.rs"]
mod ui_visual_review;
#[path = "../ui_web_gpu_storage.rs"]
mod ui_web_gpu_storage;
#[path = "../ui_web_job_gpu.rs"]
mod ui_web_job_gpu;
#[path = "../ui_web_offload.rs"]
mod ui_web_offload;
#[path = "../ui_workflow_selection.rs"]
mod ui_workflow_selection;
#[path = "../viewer_passive_display.rs"]
mod viewer_passive_display;
