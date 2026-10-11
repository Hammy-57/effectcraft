//! Smart Freeze command, snapshot and pixel regressions using original synthetic content.

use std::sync::Arc;

use effectcraft_project::freeze::{Section, Target};
use effectcraft_project::{ItemId, LayerId};
use effectcraft_render::{RenderOpts, Renderer};
use effectcraft_time::Tick;
use serde_json::json;

use crate::{
    Session,
    freeze::{Capture, Preview},
};

fn fixture() -> Session {
    let mut session = Session::default();
    session.execute("comp.new", json!({"name":"Freeze fixture", "width":16, "height":16, "frameRate":10, "duration":0.2})).unwrap();
    session.execute("layer.newSolid", json!({"name":"Red", "color":"#ff0000", "width":8, "height":8})).unwrap();
    session
}

fn section(s: &Session) -> Section {
    Section {
        id: 99,
        name: "Test".into(),
        target: Target::Comp { comp: s.active_comp_id().unwrap() },
        start: Tick::ZERO,
        end: Tick::from_seconds_f64(0.2),
        follow_comp_range: false,
        enabled: true,
        auto: false,
        quick_draft: false,
        cache: None,
        previous: vec![],
    }
}

fn pixels(s: &Session, frozen: bool) -> Vec<[f32; 4]> {
    let preview = Preview::new(&s.project);
    let mut renderer = Renderer::new(&s.project, s.footage.as_ref(), RenderOpts { guides: true, ..Default::default() });
    renderer.freeze = frozen.then_some(&preview);
    renderer.comp_frame(s.active_comp_id().unwrap(), Tick::ZERO).data
}

#[test]
fn freeze_manual_roundtrip_preserves_originals_and_export() {
    let mut s = fixture();
    let before = s.project.items.clone();
    let live = pixels(&s, false);
    let result = s.execute("freeze.freeze", json!({"mode":"comp", "wait":true, "quickDraft":false})).unwrap();
    let id = result["section"].as_u64().unwrap();
    assert_eq!(before, s.project.items);
    assert_eq!(pixels(&s, true), live);
    let cached = s.project.freeze.sections[0].cache.clone().unwrap();
    s.execute("layer.newSolid", json!({"name":"Blue", "color":"#0000ff"})).unwrap();
    assert_eq!(pixels(&s, true), live, "manual freeze deliberately stays unchecked until Edit");
    assert_ne!(pixels(&s, false), live, "exports never consult the preview cache");
    s.execute("freeze.edit", json!({"section":id})).unwrap();
    assert_eq!(pixels(&s, true), pixels(&s, false));
    assert!(s.undo());
    assert_eq!(pixels(&s, true), live);
    std::fs::remove_file(std::path::Path::new(&cached.directory).join("0.ecfreeze")).unwrap();
    assert_eq!(pixels(&s, true), pixels(&s, false), "a missing cache frame restores originals");
    std::fs::remove_dir_all(cached.directory).unwrap();
}

#[test]
fn snapshot_dependency_check_allows_unrelated_comp_but_rejects_changed_source() {
    let mut s = fixture();
    let target = section(&s);
    let capture = Capture::new(s.project.clone(), &target).unwrap();
    s.execute("comp.new", json!({"name":"Unrelated", "width":16, "height":16, "duration":1})).unwrap();
    assert!(capture.still_matches(&s.project));
    s.execute("comp.open", json!({"comp":target.target.comp().0})).unwrap();
    s.execute("layer.newSolid", json!({"color":"#0000ff"})).unwrap();
    assert!(!capture.still_matches(&s.project));
}

#[test]
fn selection_stays_in_same_timeline_and_rejects_nonadjacent_layers() {
    let mut s = fixture();
    for name in ["Second", "Third"] {
        s.execute("layer.newSolid", json!({"name":name})).unwrap();
    }
    let ids: Vec<_> = s.active_comp().unwrap().layers.iter().map(|l| l.id).collect();
    let mut sec = section(&s);
    sec.target = Target::Selection { comp: sec.target.comp(), layers: vec![ids[0], ids[2]] };
    assert!(Capture::new(s.project.clone(), &sec).is_err());
    sec.target = Target::Selection { comp: sec.target.comp(), layers: vec![ids[0], ids[1]] };
    let capture = Capture::new(s.project.clone(), &sec).unwrap();
    let original = s.active_comp().unwrap();
    let rendered = capture.render.comp(sec.target.comp()).unwrap();
    assert_eq!(original.layers.len(), rendered.layers.len());
    for (a, b) in original.layers.iter().zip(rendered.layers.iter()) {
        assert_eq!(a.id, b.id);
        assert_eq!(a.name, b.name);
        assert_eq!(a.props, b.props);
        assert_eq!(a.switches.audio, b.switches.audio);
    }
    assert!(rendered.layer(ids[2]).unwrap().switches.video, "selection rendering preserves the original scene context");
    assert!(original.layer(ids[2]).unwrap().switches.video);
}

#[test]
fn malformed_target_returns_error() {
    let s = fixture();
    let mut sec = section(&s);
    sec.target = Target::Selection { comp: ItemId(u64::MAX), layers: vec![LayerId(u64::MAX)] };
    assert!(Capture::new(Arc::clone(&s.project), &sec).is_err());
}

#[test]
fn freeze_cyclic_precomps_report_an_error_instead_of_rendering_into_recursion() {
    let mut s = Session::default();
    s.replace_project(effectcraft_project::Project::from_json(include_str!("../../../tests/repros/freeze-cycle.ecproj")).unwrap(), None);
    let error = s.execute("freeze.freeze", json!({"mode":"comp","wait":true})).unwrap_err();
    assert!(error.to_string().contains("cycle"), "{error}");
    assert_eq!(s.project.items.len(), 3);
}

#[test]
fn freeze_selection_tracks_layer_links_but_ignores_unrelated_layer_edits() {
    let mut s = fixture();
    let selected = s.active_comp().unwrap().layers[0].id;
    let reference = LayerId(s.execute("layer.newSolid", json!({"name":"Matte"})).unwrap()["layer"].as_u64().unwrap());
    let unrelated = LayerId(s.execute("layer.newSolid", json!({"name":"Unrelated"})).unwrap()["layer"].as_u64().unwrap());
    let cid = s.active_comp_id().unwrap();
    s.execute("effect.apply", json!({"layers":[selected.0],"effect":"ec.channel.setmatte"})).unwrap();
    s.edit("Link matte", None, |p, _| {
        p.comp_mut(cid).unwrap().layer_mut(selected).unwrap().props.walk_mut(&mut |prop| {
            if prop.match_id == "takeMatteFromLayer" {
                prop.value = effectcraft_keyframe::Value::Layer(Some(reference.0));
            }
        });
        Ok(())
    })
    .unwrap();
    let mut sec = section(&s);
    sec.target = Target::Selection { comp: cid, layers: vec![selected] };
    let captured = Capture::new(s.project.clone(), &sec).unwrap();
    let source = s.active_comp().unwrap().layer(reference).unwrap().source.item().unwrap();
    assert!(captured.items.contains(&source), "linked footage/solid sources must be in the identity");
    s.edit("Unrelated edit", None, |p, _| {
        p.comp_mut(cid).unwrap().layer_mut(unrelated).unwrap().props.prop_mut("transform/position").unwrap().value =
            effectcraft_keyframe::Value::Vec2([5.0, 7.0]);
        Ok(())
    })
    .unwrap();
    assert!(captured.still_matches(&s.project));
    assert_eq!(captured.identity, Capture::new(s.project.clone(), &sec).unwrap().identity);
    s.edit("Change referenced source", None, |p, _| {
        if let effectcraft_project::ItemKind::Solid(solid) = &mut p.item_mut(source).unwrap().kind {
            solid.color = [0.0, 0.0, 0.0];
        }
        Ok(())
    })
    .unwrap();
    assert!(!captured.still_matches(&s.project));
    assert_ne!(captured.identity, Capture::new(s.project.clone(), &sec).unwrap().identity);
}

#[test]
fn freeze_literal_control_link_does_not_require_unrelated_missing_media() {
    let mut s = fixture();
    let sec = section(&s);
    let cid = sec.target.comp();
    s.execute("comp.new", json!({"name":"Controls","width":16,"height":16,"duration":1})).unwrap();
    let controls = s.active_comp_id().unwrap();
    s.execute("layer.newNull", json!({"name":"CTRL"})).unwrap();
    s.execute("effect.apply", json!({"effect":"ec.control.slider"})).unwrap();
    let missing = s
        .edit("Expression and unused footage", None, |p, _| {
            p.comp_mut(cid).unwrap().layers[0].props.prop_mut("transform/opacity").unwrap().expr =
                Some(effectcraft_project::Expression { enabled: true, text: "comp('Controls').layer('CTRL').effect('Slider Control')('Slider')".into() });
            Ok(p.add_item(
                "Unused missing",
                effectcraft_color::Label::Red,
                None,
                effectcraft_project::ItemKind::Footage(effectcraft_project::Footage {
                    path: "/nonexistent/smart-freeze-unused.mov".into(),
                    ..Default::default()
                }),
            ))
        })
        .unwrap();
    let captured = Capture::new(s.project.clone(), &sec).unwrap();
    assert!(captured.items.contains(&controls));
    assert!(!captured.items.contains(&missing));
}

#[test]
fn freeze_selection_keeps_unselected_camera_context() {
    let mut s = fixture();
    let lid = s.active_comp().unwrap().layers[0].id;
    s.execute("effect.apply", json!({"layers":[lid.0],"effect":"ec.sim.ccparticleworld"})).unwrap();
    s.execute("layer.newCamera", json!({})).unwrap();
    let mut sec = section(&s);
    sec.target = Target::Selection { comp: sec.target.comp(), layers: vec![lid] };
    let captured = Capture::new(s.project.clone(), &sec).unwrap();
    assert!(Arc::ptr_eq(&captured.original, &captured.render));
    assert!(captured.render.comp(sec.target.comp()).unwrap().active_camera(Tick::ZERO).is_some());
    assert_eq!(captured.layer_dependencies.as_ref().unwrap().len(), 2);
}

#[test]
fn freeze_draft_full_and_live_audio_roundtrip() {
    let mut s = fixture();
    let cid = s.active_comp_id().unwrap();
    let lid = s.active_comp().unwrap().layers[0].id;
    s.execute("effect.apply", json!({"layers":[lid.0],"effect":"Tone"})).unwrap();
    let mix = |s: &Session| effectcraft_render::audio::mix_comp(&s.project, s.footage.as_ref(), None, cid, Tick::ZERO, 4800, 48000);
    let before = mix(&s);
    assert!(before.iter().any(|v| v.abs() > 0.01));
    s.execute("freeze.freeze", json!({"mode":"comp","wait":true,"quickDraft":true})).unwrap();
    let section = &s.project.freeze.sections[0];
    assert!(!section.cache.as_ref().unwrap().draft);
    assert!(section.previous.iter().any(|v| v.draft));
    assert_eq!(mix(&s), before, "the original audio continues through a frozen visual preview");
    assert_eq!(pixels(&s, true), pixels(&s, false));
    let cached = section.cache.as_ref().unwrap().directory.clone();
    let id = section.id;
    s.execute("freeze.auto", json!({"section":id,"enabled":true})).unwrap();
    s.freeze_runtime.activity = web_time::Instant::now() - std::time::Duration::from_secs(10);
    s.poll_freeze();
    s.wait_jobs();
    assert!(s.freeze_preview_project().freeze.sections[0].cache.is_some());
    // Actual failed frame reads are reported back to the UI/runtime; a stale fault cannot
    // invalidate some later cache version with the same section ID.
    std::fs::remove_file(std::path::Path::new(&cached).join("0.ecfreeze")).unwrap();
    let projected = s.freeze_preview_project();
    let preview = Preview::new(&projected).with_feedback(&s.freeze_runtime.feedback);
    let mut renderer = Renderer::new(&projected, s.footage.as_ref(), RenderOpts { guides: true, ..Default::default() });
    renderer.freeze = Some(&preview);
    renderer.comp_frame(cid, Tick::ZERO);
    s.execute("freeze.auto", json!({"section":id,"enabled":false})).unwrap();
    s.poll_freeze();
    assert!(s.freeze_preview_project().freeze.sections[0].cache.is_none());
    assert!(s.freeze_runtime.reasons[&id].contains("Cache unavailable"));
    s.freeze_runtime.feedback.lock().unwrap().push(crate::freeze::Fault {
        namespace: s.project.freeze.namespace.clone(),
        section: id,
        directory: "old-version".into(),
        message: "stale".into(),
    });
    s.poll_freeze();
    assert!(!s.freeze_runtime.bad_versions.contains("old-version"));
    std::fs::remove_dir_all(crate::freeze_storage::root(&s.project.freeze.namespace).unwrap()).unwrap();
}

struct GateFootage {
    entered: std::sync::mpsc::Sender<()>,
    release: (std::sync::Mutex<bool>, std::sync::Condvar),
    calls: std::sync::atomic::AtomicUsize,
    block_at: std::sync::atomic::AtomicUsize,
}
impl GateFootage {
    fn release(&self) {
        *self.release.0.lock().unwrap() = true;
        self.release.1.notify_all();
    }
}
impl effectcraft_render::FootageSource for GateFootage {
    fn frame(&self, _: ItemId, _: &effectcraft_project::Footage, _: Tick) -> Option<Arc<effectcraft_render::Image>> {
        if self.calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed) >= self.block_at.load(std::sync::atomic::Ordering::Relaxed) {
            let _ = self.entered.send(());
            let mut released = self.release.0.lock().unwrap();
            while !*released {
                released = self.release.1.wait(released).unwrap();
            }
        }
        Some(Arc::new(effectcraft_render::Image { width: 2, height: 2, data: vec![[0.5, 0.0, 0.0, 0.5]; 4] }))
    }
}

fn gated(block_at: usize) -> (Session, Arc<GateFootage>, std::sync::mpsc::Receiver<()>, std::path::PathBuf) {
    let mut s = fixture();
    let cid = s.active_comp_id().unwrap();
    let path = std::env::temp_dir().join(format!(
        "freeze-gate-{}-{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    std::fs::write(&path, b"synthetic footage source").unwrap();
    let item = s
        .edit("Synthetic footage", None, |p, _| {
            Ok(p.add_item(
                "Gate",
                effectcraft_color::Label::Red,
                None,
                effectcraft_project::ItemKind::Footage(effectcraft_project::Footage {
                    path: path.to_string_lossy().into_owned(),
                    width: 2,
                    height: 2,
                    has_video: true,
                    duration: Tick::from_seconds_f64(1.0),
                    ..Default::default()
                }),
            ))
        })
        .unwrap();
    s.execute("layer.addItem", json!({"item":item.0,"comp":cid.0})).unwrap();
    let (send, recv) = std::sync::mpsc::channel();
    let source = Arc::new(GateFootage {
        entered: send,
        release: (std::sync::Mutex::new(false), std::sync::Condvar::new()),
        calls: Default::default(),
        block_at: std::sync::atomic::AtomicUsize::new(block_at),
    });
    s.footage = source.clone();
    (s, source, recv, path)
}

#[test]
fn freeze_cancel_full_render_keeps_successful_draft() {
    let (mut s, source, entered, path) = gated(usize::MAX);
    s.execute("freeze.freeze", json!({"mode":"comp","quickDraft":true})).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while s.jobs().iter().any(|j| j.kind == "smartFreeze" && j.running) && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let slot = s.freeze_runtime.render_slot.clone();
    let guard = slot.lock().unwrap();
    s.poll_jobs();
    let draft = s.project.freeze.sections[0].cache.clone();
    source.block_at.store(source.calls.load(std::sync::atomic::Ordering::Relaxed), std::sync::atomic::Ordering::Relaxed);
    drop(guard);
    let full_entered = entered.recv_timeout(std::time::Duration::from_secs(5));
    s.execute("freeze.cancel", json!({})).unwrap();
    source.release();
    s.wait_jobs();
    assert!(draft.as_ref().is_some_and(|v| v.draft));
    assert!(full_entered.is_ok());
    assert_eq!(s.project.freeze.sections[0].cache, draft);
    assert!(s.job_log.iter().any(|j| j.kind == "smartFreeze" && j.status == "cancelled"));
    std::fs::remove_dir_all(crate::freeze_storage::root(&s.project.freeze.namespace).unwrap()).unwrap();
    std::fs::remove_file(path).unwrap();
}

#[test]
fn freeze_project_switch_does_not_wait_for_a_blocked_frame_or_attach_it_later() {
    let (mut s, source, entered, path) = gated(0);
    s.execute("freeze.freeze", json!({"mode":"comp","quickDraft":false})).unwrap();
    let started = entered.recv_timeout(std::time::Duration::from_secs(10));
    let namespace = s.project.freeze.namespace.clone();
    let slot = s.freeze_runtime.render_slot.clone();
    let begin = std::time::Instant::now();
    s.replace_project(Default::default(), None);
    let elapsed = begin.elapsed();
    source.release();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while slot.try_lock().is_err() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(started.is_ok());
    assert!(elapsed < std::time::Duration::from_secs(1), "project switch took {elapsed:?}");
    assert!(slot.try_lock().is_ok());
    s.poll_jobs();
    assert!(s.project.items.is_empty());
    assert!(s.project.freeze.sections.is_empty());
    std::fs::remove_dir_all(crate::freeze_storage::root(&namespace).unwrap()).unwrap();
    std::fs::remove_file(path).unwrap();
}

#[test]
fn freeze_control_waiter_follows_draft_to_full_even_after_the_draft_log_is_removed() {
    let (mut s, source, entered, path) = gated(usize::MAX);
    let crate::job_waiter::Started::Waiting(waiter) =
        crate::job_waiter::start(&mut s, "freeze.freeze", json!({"mode":"comp","wait":true,"quickDraft":true})).unwrap()
    else {
        panic!("expected background waiter")
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while s.jobs().iter().any(|j| j.kind == "smartFreeze" && j.running) && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(s.jobs().iter().all(|j| j.kind != "smartFreeze" || !j.running));
    let slot = s.freeze_runtime.render_slot.clone();
    let guard = slot.lock().unwrap();
    s.poll_jobs();
    source.block_at.store(source.calls.load(std::sync::atomic::Ordering::Relaxed), std::sync::atomic::Ordering::Relaxed);
    drop(guard);
    let started = entered.recv_timeout(std::time::Duration::from_secs(5));
    let follows = waiter.pending(&s);
    s.job_log.retain(|j| !waiter.jobs.contains(&j.id));
    let retained = waiter.pending(&s);
    source.release();
    s.wait_jobs();
    assert!(started.is_ok() && follows && retained);
    assert!(!waiter.pending(&s));
    let result = waiter.finish(&mut s);
    assert_eq!(result["ok"], true, "{result}");
    assert_eq!(result["result"]["version"]["draft"], false);
    std::fs::remove_dir_all(crate::freeze_storage::root(&s.project.freeze.namespace).unwrap()).unwrap();
    std::fs::remove_file(path).unwrap();
}

#[test]
fn freeze_automatic_refresh_does_not_swallow_the_authoring_undo() {
    let mut s = fixture();
    let cid = s.active_comp_id().unwrap();
    let lid = s.active_comp().unwrap().layers[0].id;
    s.execute("freeze.freeze", json!({"wait":true,"quickDraft":false})).unwrap();
    let id = s.project.freeze.sections[0].id;
    s.execute("freeze.auto", json!({"section":id,"enabled":true})).unwrap();
    let before = s.project.comp(cid).unwrap().layer(lid).unwrap().props.prop("transform/position").unwrap().value.clone();
    s.execute("prop.set", json!({"layer":lid.0,"path":"transform/position","value":[3,4,0]})).unwrap();
    let undo_count = s.history.undo.len();
    s.execute("freeze.refresh", json!({"section":id,"wait":true,"automatic":true})).unwrap();
    assert_eq!(s.history.undo.len(), undo_count, "automatic preview data must not add an authoring undo step");
    assert!(s.undo());
    assert_eq!(s.project.comp(cid).unwrap().layer(lid).unwrap().props.prop("transform/position").unwrap().value, before);
    std::fs::remove_dir_all(crate::freeze_storage::root(&s.project.freeze.namespace).unwrap()).unwrap();
}

#[test]
fn freeze_auto_waits_for_eight_seconds_after_recent_interaction() {
    let mut s = fixture();
    let mut target = section(&s);
    target.auto = true;
    Arc::make_mut(&mut s.project).freeze.sections.push(target);
    Arc::make_mut(&mut s.project).freeze.namespace = format!("project-auto-idle-{}-{}", std::process::id(), crate::freeze_runtime::now_millis());
    s.freeze_runtime.changed = web_time::Instant::now() - std::time::Duration::from_secs(30);
    s.freeze_runtime.activity = web_time::Instant::now() - std::time::Duration::from_secs(4);
    s.poll_freeze();
    s.wait_jobs();
    assert!(s.project.freeze.sections[0].cache.is_none(), "recent interaction must defer rendering even when the edit is old");
    s.freeze_runtime.activity = web_time::Instant::now() - std::time::Duration::from_secs(9);
    s.freeze_runtime.retry_after.clear();
    s.poll_freeze();
    s.wait_jobs();
    assert!(s.project.freeze.sections[0].cache.is_some(), "Auto should refresh after the full idle interval");
    std::fs::remove_dir_all(crate::freeze_storage::root(&s.project.freeze.namespace).unwrap()).unwrap();
}

#[test]
fn freeze_default_range_follows_duration_but_explicit_ranges_stay_fixed() {
    let mut s = fixture();
    s.execute("freeze.freeze", json!({"wait":true,"quickDraft":false})).unwrap();
    let id = s.project.freeze.sections[0].id;
    assert_eq!(s.project.freeze.sections[0].cache.as_ref().unwrap().frames, 2);
    s.execute("comp.settings", json!({"duration":0.4})).unwrap();
    s.execute("freeze.refresh", json!({"section":id,"wait":true})).unwrap();
    assert_eq!(s.project.freeze.sections[0].cache.as_ref().unwrap().frames, 4);
    assert_eq!(s.project.freeze.sections[0].end, Tick::from_seconds_f64(0.4));
    s.execute("freeze.auto", json!({"section":id,"enabled":true})).unwrap();
    s.execute("comp.settings", json!({"duration":0.3})).unwrap();
    let undo = s.history.undo.len();
    s.execute("freeze.refresh", json!({"section":id,"wait":true,"automatic":true})).unwrap();
    assert_eq!(s.project.freeze.sections[0].cache.as_ref().unwrap().frames, 3);
    assert_eq!(s.project.freeze.sections[0].end, Tick::from_seconds_f64(0.3));
    assert_eq!(s.history.undo.len(), undo);
    s.execute("freeze.freeze", json!({"start":0.1,"end":0.2,"wait":true,"quickDraft":false})).unwrap();
    assert_eq!(s.project.freeze.sections.len(), 1, "a repeated target updates its existing range");
    assert!(!s.project.freeze.sections[0].follow_comp_range);
    s.execute("comp.settings", json!({"duration":0.5})).unwrap();
    s.execute("freeze.refresh", json!({"section":id,"wait":true})).unwrap();
    assert_eq!(s.project.freeze.sections[0].cache.as_ref().unwrap().frames, 1);
    s.execute("freeze.configure", json!({"section":id,"followCompositionRange":true})).unwrap();
    s.execute("freeze.refresh", json!({"section":id,"wait":true})).unwrap();
    assert_eq!(s.project.freeze.sections[0].cache.as_ref().unwrap().frames, 5);
    std::fs::remove_dir_all(crate::freeze_storage::root(&s.project.freeze.namespace).unwrap()).unwrap();
}

#[test]
fn freeze_refresh_and_auto_verification_discard_stale_decoded_media() {
    #[derive(Default)]
    struct CachedFile(std::sync::Mutex<Option<Arc<effectcraft_render::Image>>>);
    impl effectcraft_render::FootageSource for CachedFile {
        fn frame(&self, _: ItemId, footage: &effectcraft_project::Footage, _: Tick) -> Option<Arc<effectcraft_render::Image>> {
            let mut cached = self.0.lock().unwrap();
            if cached.is_none() {
                let bytes = std::fs::read(&footage.path).ok()?;
                let color = if bytes.first() == Some(&1) { [0.0, 0.0, 1.0, 1.0] } else { [1.0, 0.0, 0.0, 1.0] };
                let mut image = effectcraft_render::Image::new(16, 16);
                image.data.fill(color);
                *cached = Some(Arc::new(image));
            }
            cached.clone()
        }
        fn forget(&self, _: &str) {
            *self.0.lock().unwrap() = None;
        }
    }
    let mut s = fixture();
    let file = std::env::temp_dir().join(format!("freeze-reload-{}-{}.bin", std::process::id(), crate::freeze_runtime::now_millis()));
    std::fs::write(&file, [0]).unwrap();
    let item = s
        .edit("Synthetic cached media", None, |p, _| {
            Ok(p.add_item(
                "Cached file",
                effectcraft_color::Label::Red,
                None,
                effectcraft_project::ItemKind::Footage(effectcraft_project::Footage {
                    path: file.to_string_lossy().into_owned(),
                    width: 16,
                    height: 16,
                    has_video: true,
                    duration: Tick::from_seconds_f64(0.2),
                    ..Default::default()
                }),
            ))
        })
        .unwrap();
    s.execute("layer.addItem", json!({"item":item.0})).unwrap();
    s.footage = Arc::new(CachedFile::default());
    let red = pixels(&s, false);
    s.execute("freeze.freeze", json!({"wait":true,"quickDraft":false})).unwrap();
    let id = s.project.freeze.sections[0].id;
    std::fs::write(&file, [1, 1]).unwrap();
    assert_eq!(pixels(&s, false), red, "fixture retains stale decoded bytes until forgotten");
    s.execute("freeze.refresh", json!({"section":id,"wait":true})).unwrap();
    assert_ne!(pixels(&s, true), red, "Refresh must bake the changed file, not its old decoded frame");
    let directory = s.project.freeze.sections[0].cache.as_ref().unwrap().directory.clone();
    s.execute("freeze.auto", json!({"section":id,"enabled":true})).unwrap();
    std::fs::write(&file, [0, 0, 0]).unwrap();
    s.freeze_runtime.proofs.get_mut(&id).unwrap().checked = web_time::Instant::now() - std::time::Duration::from_secs(16);
    s.freeze_runtime.activity = web_time::Instant::now() - std::time::Duration::from_secs(4);
    s.poll_freeze();
    s.wait_jobs();
    assert_eq!(pixels(&s, false), red, "Auto's live originals must use freshly decoded media while refresh waits for idle");
    assert_eq!(s.project.freeze.sections[0].cache.as_ref().unwrap().directory, directory);
    std::fs::remove_dir_all(crate::freeze_storage::root(&s.project.freeze.namespace).unwrap()).unwrap();
    std::fs::remove_file(file).unwrap();
}

#[test]
fn freeze_pixel_epochs_are_scoped_to_the_affected_composition() {
    let mut s = fixture();
    let first = s.active_comp_id().unwrap();
    s.execute("freeze.freeze", json!({"wait":true,"quickDraft":false})).unwrap();
    let first_section = s.project.freeze.sections[0].id;
    let before = s.freeze_runtime.pixel_epoch(&s.project, first);
    s.execute("comp.new", json!({"name":"Other","width":16,"height":16,"duration":0.2,"frameRate":10})).unwrap();
    s.execute("freeze.freeze", json!({"wait":true,"quickDraft":false})).unwrap();
    let second = s.project.freeze.sections[1].id;
    let capture = s.freeze_runtime.proofs[&second].capture.clone();
    s.freeze_runtime.accept(second, s.revision, capture);
    assert_eq!(s.freeze_runtime.pixel_epoch(&s.project, first), before, "checking another section must not evict this composition");
    let revision = s.revision;
    s.freeze_runtime.invalidate_section(first_section);
    assert_ne!(s.freeze_runtime.pixel_epoch(&s.project, first), before);
    assert_eq!(s.revision, revision, "derived preview invalidation is not a document edit");
    std::fs::remove_dir_all(crate::freeze_storage::root(&s.project.freeze.namespace).unwrap()).unwrap();
}

#[test]
fn freeze_partial_refresh_reuses_unaffected_visible_frames() {
    let mut s = fixture();
    let cid = s.active_comp_id().unwrap();
    let changed = s.active_comp().unwrap().layers[0].id;
    s.execute("comp.settings", json!({"duration":1.0})).unwrap();
    s.execute("layer.newSolid", json!({"name":"Unchanged","width":2,"height":2,"color":"#00ff00"})).unwrap();
    let ids: Vec<_> = s.active_comp().unwrap().layers.iter().map(|l| l.id.0).collect();
    s.execute("freeze.freeze", json!({"mode":"selection","layers":ids,"wait":true,"quickDraft":false})).unwrap();
    let id = s.project.freeze.sections[0].id;
    s.execute("prop.set", json!({"layer":changed.0,"path":"transform/position","value":[2,3,0]})).unwrap();
    s.execute("freeze.refresh", json!({"section":id,"wait":true})).unwrap();
    assert!(s.project.freeze.sections[0].cache.as_ref().unwrap().reused_frames > 0);
    let preview = Preview::new(&s.project);
    let mut r = Renderer::new(&s.project, s.footage.as_ref(), RenderOpts { guides: true, ..Default::default() });
    let live = r.comp_frame(cid, Tick::from_seconds_f64(0.5));
    r.freeze = Some(&preview);
    assert_eq!(
        r.comp_frame(cid, Tick::from_seconds_f64(0.5)).data,
        live.data,
        "inactive selected members must not prevent reuse of the remaining visible member"
    );
    std::fs::remove_dir_all(crate::freeze_storage::root(&s.project.freeze.namespace).unwrap()).unwrap();
}

#[test]
fn freeze_121_manual_sections_do_not_poll_media_or_copy_the_project() {
    use effectcraft_project::{Comp, ItemKind, freeze::Version};
    use effectcraft_time::FrameRate;
    let mut project = effectcraft_project::Project::default();
    for i in 0..121 {
        let comp = project.add_item(
            &format!("Section {i}"),
            Default::default(),
            None,
            ItemKind::Comp(Comp::new(16, 16, FrameRate::FPS_30, Tick::from_seconds_f64(1.0)).into()),
        );
        project.freeze.sections.push(Section {
            id: 10000 + i,
            name: format!("Section {i}"),
            target: Target::Comp { comp },
            start: Tick::ZERO,
            end: Tick::from_seconds_f64(1.0),
            follow_comp_range: false,
            enabled: true,
            auto: false,
            quick_draft: false,
            previous: vec![],
            cache: Some(Version {
                directory: format!("/nonexistent/freeze-bench/{i}"),
                identity: "benchmark".into(),
                draft: false,
                width: 16,
                height: 16,
                frames: 30,
                bytes: 0,
                created_unix: 0,
                render_options: serde_json::to_string(&RenderOpts::default()).unwrap(),
                frame_rate: FrameRate::FPS_30,
                sample: None,
                reused_frames: 0,
            }),
        });
    }
    let mut s = Session::default();
    s.replace_project(project, None);
    let original = s.project.clone();
    let started = std::time::Instant::now();
    for _ in 0..10000 {
        s.poll_freeze();
        assert!(Arc::ptr_eq(&original, &s.freeze_preview_project()));
    }
    eprintln!("121 manual sections, 10000 heartbeat/view requests: {:.3} ms", started.elapsed().as_secs_f64() * 1000.0);
    assert!(s.tasks.is_empty());
    assert!(s.freeze_runtime.proofs.is_empty());
    assert!(s.freeze_runtime.reasons.is_empty());
}

#[test]
fn freeze_cropped_preview_and_subframe_fallback_preserve_render_context() {
    use effectcraft_render::freeze::FreezeSource;
    let mut s = fixture();
    let comp = s.active_comp_id().unwrap();
    s.execute("freeze.freeze", json!({"wait":true,"quickDraft":false})).unwrap();
    let preview = Preview::new(&s.project);
    let opts = RenderOpts { guides: true, roi: Some([2.0, 2.0, 8.0, 8.0]), ..Default::default() };
    assert!(preview.frame(comp, &[], Tick::ZERO, &opts).is_some());
    let mut r = Renderer::new(&s.project, s.footage.as_ref(), opts);
    let live = r.comp_frame(comp, Tick::ZERO);
    r.freeze = Some(&preview);
    assert_eq!(r.comp_frame(comp, Tick::ZERO).data, live.data);
    assert!(preview.frame(comp, &[], Tick::from_seconds_f64(0.05), &opts).is_none(), "a baked frame must not replace subframe evaluation");
    std::fs::remove_dir_all(crate::freeze_storage::root(&s.project.freeze.namespace).unwrap()).unwrap();
}
