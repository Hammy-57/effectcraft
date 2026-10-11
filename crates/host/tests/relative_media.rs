//! #652: a Dock launch has a different CWD from a CLI launched beside the project.
use effectcraft_engine::project::ItemKind;
use effectcraft_engine::render::{RenderOpts, audio::mix_comp};
use effectcraft_time::Tick;
use serde_json::json;
use std::sync::Arc;

fn wav() -> Vec<u8> {
    let pcm: Vec<u8> = (0..12_000).flat_map(|i| ((if i % 100 < 50 { 8_000 } else { -8_000 }) as i16).to_le_bytes()).collect();
    let mut b = b"RIFF".to_vec();
    b.extend((36 + pcm.len() as u32).to_le_bytes());
    b.extend(b"WAVEfmt ");
    b.extend(16u32.to_le_bytes());
    b.extend(1u16.to_le_bytes());
    b.extend(1u16.to_le_bytes());
    b.extend(48_000u32.to_le_bytes());
    b.extend(96_000u32.to_le_bytes());
    b.extend(2u16.to_le_bytes());
    b.extend(16u16.to_le_bytes());
    b.extend(b"data");
    b.extend((pcm.len() as u32).to_le_bytes());
    b.extend(pcm);
    b
}

#[test]
fn opened_relative_video_and_audio_reach_preview_and_queued_export() {
    let dir = std::env::temp_dir().join(format!("effectcraft-relative-media-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let movie = dir.join("background.mp4");
    let sound = dir.join("sound.wav");
    std::fs::write(&sound, wav()).unwrap();
    // Generate the H.264 fixture with the project's own exporter; no binary assets in git.
    let mut seed = effectcraft_host::session();
    seed.execute("comp.new", json!({"width": 64, "height": 48, "frameRate": 12, "duration": 0.25})).unwrap();
    seed.execute("layer.newSolid", json!({"color": "#ff0000"})).unwrap();
    seed.execute("renderQueue.add", json!({"format": "h264", "output": movie})).unwrap();
    seed.execute("renderQueue.render", json!({"wait": true})).unwrap();
    assert_eq!(seed.project.render_queue[0].status.label(), "Done");

    let mut s = effectcraft_host::session();
    s.execute("comp.new", json!({"width": 64, "height": 48, "frameRate": 12, "duration": 0.25})).unwrap();
    let imported = s.execute("file.import", json!({"paths": [movie, sound]})).unwrap();
    assert_eq!(imported["errors"], json!([]));
    for item in imported["items"].as_array().unwrap() {
        s.execute("layer.addItem", json!({"item": item})).unwrap();
    }
    for item in Arc::make_mut(&mut s.project).items.values_mut() {
        if let ItemKind::Footage(f) = &mut item.kind {
            f.path = std::path::Path::new(&f.path).file_name().unwrap().to_str().unwrap().into();
        }
    }
    let project = dir.join("show.ecproj");
    std::fs::write(&project, s.project.to_json()).unwrap();
    // Do not change process CWD: this runs beside other tests, as the app does from the Dock.
    let mut app = effectcraft_host::session();
    app.check_footage_on_open = true;
    app.execute("file.open", json!({"path": project})).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while app.jobs().iter().any(|j| j.kind == "footageCheck") && std::time::Instant::now() < deadline {
        app.poll_jobs();
        std::thread::yield_now();
    }
    for item in app.project.items.values() {
        if let ItemKind::Footage(f) = &item.kind {
            assert!(!f.missing, "{} was incorrectly marked missing", f.path);
            assert!(std::path::Path::new(&f.path).is_absolute());
        }
    }
    let comp = app.active_comp_id().unwrap();
    let frame = app.render(comp, Tick::ZERO, RenderOpts::default());
    assert!(frame.data.iter().any(|p| p[0] > 0.8 && p[1] < 0.2 && p[3] > 0.9));
    let audio = mix_comp(&app.project, app.footage.as_ref(), app.expr.as_deref(), comp, Tick::ZERO, 4096, 48_000);
    assert!(audio.iter().any(|v| v.abs() > 0.1));
    let output = dir.join("output.mp4");
    app.execute("renderQueue.add", json!({"format": "h264", "output": output})).unwrap();
    app.execute("renderQueue.render", json!({"wait": true})).unwrap();
    assert_eq!(app.project.render_queue[0].status.label(), "Done");
    let f = effectcraft_media::probe(&output).unwrap();
    assert!(f.has_video && f.has_audio, "the queued MP4 must contain picture and AAC audio");
    let pool = effectcraft_media::MediaPool::new();
    assert!(pool.frame_at(&f, Tick::ZERO).unwrap().data.iter().any(|p| p[0] > 0.8 && p[1] < 0.2));
    assert!(pool.audio_samples(&f, Tick::ZERO, 4096, 48_000).iter().any(|v| v.abs() > 0.05));
    let _ = std::fs::remove_dir_all(dir);
}
