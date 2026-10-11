//! The viewer on the GPU compositor (egui_kittest with wgpu): frames composited on the GPU match
//! the CPU viewer. Skips without a GPU adapter (CI). Set `EC_SNAPSHOT_DIR` to keep PNGs.

use effectcraft_engine::Session;
use effectcraft_ui_egui::EffectcraftApp;
use egui_kittest::Harness;
use serde_json::json;

fn settle(h: &mut Harness<'_, EffectcraftApp>) {
    for _ in 0..600 {
        h.step();
        if h.state().frames.inflight() == 0 && h.state().frames.last_ms.lock().map(|v| *v > 0.0).unwrap_or(false) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    for _ in 0..4 {
        h.step();
    }
}

/// egui_kittest's wgpu setup on the adapter the headless compositor picked (`name (Backend)`),
/// like the desktop app's window. Left alone, egui_kittest prefers a CPU adapter, which on
/// Windows is WARP on DirectX 12, where FXC fails to compile the compositor's kernels: the
/// viewer would stay on the CPU and the comparison would never run.
fn same_adapter(name: String) -> eframe::egui_wgpu::WgpuSetup {
    let mut setup = egui_kittest::wgpu::default_wgpu_setup();
    if let eframe::egui_wgpu::WgpuSetup::CreateNew(c) = &mut setup {
        let fallback = c.native_adapter_selector.take();
        c.native_adapter_selector = Some(std::sync::Arc::new(move |adapters, surface| {
            let named = |a: &&eframe::wgpu::Adapter| format!("{} ({:?})", a.get_info().name, a.get_info().backend) == name;
            match (adapters.iter().find(named), &fallback) {
                (Some(a), _) => Ok(a.clone()),
                (None, Some(f)) => f(adapters, surface),
                (None, None) => adapters.first().cloned().ok_or_else(|| "No adapter found".to_owned()),
            }
        }));
    }
    setup
}

#[test]
fn viewer_draws_gpu_frames_that_match_the_cpu() {
    let _gpu = crate::gpu_lock();
    let Some(probe) = effectcraft_gpu::Gpu::headless() else {
        eprintln!("no GPU adapter: skipping");
        return;
    };
    let adapter = effectcraft_engine::render::Accelerator::name(&probe);
    eprintln!("headless compositor adapter: {adapter}");
    // egui-wgpu creates its GL device with WebGL2's limits, which the compositor declines.
    let gl = adapter.ends_with("(Gl)");
    drop(probe);
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    s.execute("time.set", json!({"time": 2.0})).unwrap();
    let mut h = Harness::builder()
        .with_size(egui::vec2(1400.0, 900.0))
        .with_pixels_per_point(1.0)
        .wgpu_setup(same_adapter(adapter))
        .build_eframe(|_| EffectcraftApp::new(s));
    settle(&mut h);
    // The initial CPU frame can finish before asynchronous compositor warm-up. Wait
    // for the adapter itself rather than mistaking that first frame for GPU readiness.
    if !gl {
        for _ in 0..600 {
            if h.state().gpu_adapter().is_some() {
                break;
            }
            h.step();
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        settle(&mut h);
    }
    if gl && h.state().gpu_adapter().is_none() {
        eprintln!("egui-wgpu's GL device has WebGL2 limits: the viewer stays on the CPU, skipping");
        return;
    }
    assert!(h.state().gpu_adapter().is_some(), "GPU compositor initialized");
    eprintln!("viewer compositor adapter: {:?}", h.state().gpu_adapter());
    let gpu_px = h.state_mut().viewer_pixels().expect("read back the GPU frame");
    let gpu_shot = h.render().expect("render");
    // Mercury Software Only: the same frame from the CPU.
    h.state_mut().session.execute("render.backend", json!({"backend": "cpu"})).unwrap();
    settle(&mut h);
    let cpu_px = h.state_mut().viewer_pixels().expect("CPU frame");
    let cpu_shot = h.render().expect("render");
    assert_eq!(gpu_px.size, cpu_px.size);
    let mut worst = 0i32;
    let mut over = 0usize;
    for (a, b) in gpu_px.pixels.iter().zip(&cpu_px.pixels) {
        let d = a.to_array().iter().zip(b.to_array()).map(|(x, y)| (*x as i32 - y as i32).abs()).max().unwrap_or(0);
        worst = worst.max(d);
        over += usize::from(d > 1);
    }
    assert!(over * 1000 <= gpu_px.pixels.len() * 3 && worst <= 4, "viewer GPU vs CPU: {over} pixels > 1/255, worst {worst}/255");
    if let Ok(d) = std::env::var("EC_SNAPSHOT_DIR") {
        gpu_shot.save(format!("{d}/ui_gpu_viewer.png")).unwrap();
        cpu_shot.save(format!("{d}/ui_cpu_viewer.png")).unwrap();
    }
}

/// #591: losing the compositor device must not take egui's presentation device or the
/// document with it. Exercise real device destruction, not just a synthetic error string.
#[test]
fn compositor_device_loss_keeps_the_window_rendering_and_the_document_editable() {
    let _gpu = crate::gpu_lock();
    let Some(probe) = effectcraft_gpu::Gpu::headless() else {
        assert!(std::env::var_os("EFFECTCRAFT_REQUIRE_GPU").is_none(), "GPU required for device-loss test");
        eprintln!("no GPU adapter: skipping device-loss test");
        return;
    };
    drop(probe);
    let mut s = Session::default();
    s.execute("comp.new", json!({"name": "Device recovery", "width": 64, "height": 64, "duration": 2})).unwrap();
    s.execute("layer.newSolid", json!({"name": "Preserved", "color": "#ff0000"})).unwrap();
    let project = s.project.clone();
    let mut presentation = None;
    let mut h = Harness::builder().with_size(egui::vec2(1200.0, 800.0)).wgpu().build_eframe(|cc| {
        presentation = cc.wgpu_render_state.as_ref().map(|rs| rs.device.clone());
        let mut app = EffectcraftApp::new(s);
        app.set_gpu_failure_bridge(effectcraft_ui_egui::gpu_failure::GpuFailureBridge::new(&cc.egui_ctx));
        app
    });
    settle(&mut h);
    let Some(compositor) = h.state().render_source().gpu else {
        assert!(std::env::var_os("EFFECTCRAFT_REQUIRE_GPU").is_none(), "compositor required for device-loss test");
        eprintln!("adapter cannot run the compositor: skipping device-loss test");
        return;
    };
    assert_ne!(compositor.device(), presentation.as_ref().unwrap(), "worker and window must have separate devices");
    h.render().expect("window renders before device loss");
    compositor.device().destroy();
    for _ in 0..100 {
        let _ = compositor.device().poll(eframe::wgpu::PollType::Poll);
        h.step();
        if h.state().session.accel.is_none() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(h.state().session.accel.is_none(), "lost compositor is retired");
    assert!(std::sync::Arc::ptr_eq(&project, &h.state().session.project), "device loss preserves the document");
    h.state_mut().session.execute("time.set", json!({"time": 1.0})).unwrap();
    settle(&mut h);
    let pixels = h.state_mut().viewer_pixels().expect("CPU preview after device loss");
    assert!(pixels.pixels.iter().any(|p| p.r() > 200 && p.g() < 10), "the original red layer still renders");
    h.render().expect("egui still presents after compositor device loss");
    h.state_mut().session.execute("edit.undo", json!({})).unwrap();
    assert!(h.state().session.active_comp().unwrap().layers.is_empty());
    h.state_mut().session.execute("edit.redo", json!({})).unwrap();
    assert_eq!(h.state().session.active_comp().unwrap().layers.len(), 1);
    h.render().expect("egui still presents after undo and redo");
}

/// Once the graphics device is lost, eframe's renderer cannot stage buffers and egui-wgpu
/// panicked on the main thread drawing the next frame (#591). The app now draws nothing,
/// auto-saves unsaved changes and closes its window instead.
#[test]
fn a_lost_device_draws_nothing_autosaves_and_closes() {
    let dir = std::env::temp_dir().join(format!("ec-device-lost-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut s = Session { autosave_folder_override: Some(dir.clone()), ..Default::default() };
    s.execute("comp.new", json!({"name": "Unsaved", "width": 64, "height": 64, "duration": 1})).unwrap();
    assert!(s.is_dirty());
    let mut h = Harness::builder().with_size(egui::vec2(1200.0, 800.0)).build_eframe(|_| EffectcraftApp::new(s));
    let told = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let log = told.clone();
    h.state_mut().hooks.device_lost = Some(Box::new(move |saved| log.borrow_mut().push(saved)));
    h.run_steps(3);
    assert!(!h.output().shapes.is_empty(), "the UI draws before the loss");
    let bridge = effectcraft_ui_egui::gpu_failure::GpuFailureBridge::new(&h.ctx);
    h.state_mut().set_gpu_failure_bridge(bridge.clone());
    bridge.report("GPU device lost (Unknown): Device is lost", true);
    h.step();
    let closing = h.output().viewport_output.get(&egui::ViewportId::ROOT).is_some_and(|v| v.commands.contains(&egui::ViewportCommand::Close));
    assert!(closing, "the window closes");
    assert!(h.output().shapes.is_empty(), "nothing for the lost renderer to stage");
    h.step();
    assert!(h.output().shapes.is_empty());
    let saved = h.state().session.autosave.last_path.clone().expect("unsaved changes auto-saved");
    assert!(std::path::Path::new(&saved).is_file(), "{saved}");
    assert_eq!(*told.borrow(), vec![Some(Ok(saved))], "the user is told once, with the recovery copy");
    let _ = std::fs::remove_dir_all(&dir);
}
