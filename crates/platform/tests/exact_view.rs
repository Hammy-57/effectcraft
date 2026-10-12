//! The media pool's exact view (what exports read) never decodes on the hardware, and keeps its
//! frames apart from the previews' hardware frames. Its own test binary: the decoder counters are
//! process-wide.

mod common;

use std::sync::Arc;

use effectcraft_media::MediaPool;
use effectcraft_project::ItemId;
use effectcraft_render::FootageSource;
use effectcraft_time::Tick;
use filmcraft_isobmff::{Brand, FourCc, Mp4Writer, SampleEntry, TrackConfig, WriteSample, WriterOptions};
use filmcraft_prores::{ChromaFormat, Profile};

/// A 24 fps ProRes 4444 MOV of `n` generated frames (no alpha: VideoToolbox rejects the alpha
/// FilmCraft's encoder writes, and the frames would all come from the software decoder).
fn movie(w: u32, h: u32, n: u32) -> Vec<u8> {
    let mut mux = Mp4Writer::new(std::io::Cursor::new(Vec::new()), WriterOptions::new(Brand::Mov)).expect("writer");
    let track = mux.add_track(TrackConfig::new(SampleEntry::prores(FourCc(*b"ap4h"), w as u16, h as u16), 24)).expect("track");
    for i in 0..n {
        let data = common::encode(Profile::P4444, &common::picture(w, h, ChromaFormat::Yuv444, 12, false, i));
        mux.write_sample(track, WriteSample { data: &data, duration: 1, composition_offset: 0, is_sync: true }).expect("sample");
    }
    mux.finish().expect("finish").into_inner()
}

#[test]
fn the_exact_view_never_decodes_on_the_hardware() {
    let available = matches!(effectcraft_platform::register(), effectcraft_platform::Availability::Available(_));
    let (w, h, n) = (256, 144, 6);
    let bytes: Arc<[u8]> = movie(w, h, n).into();
    let path = "/mem/hw-exact-view.mov";
    let footage = effectcraft_media::probe_bytes(path, bytes.clone()).expect("probe");
    assert!(footage.codec.contains("ProRes"), "codec {}", footage.codec);
    let at = |i: u32| footage.frame_rate.tick_of(i64::from(i)) + Tick(footage.frame_rate.frame_duration().0 / 2);

    let pool = MediaPool::new();
    pool.add_bytes(path, bytes.clone());
    pool.set_hardware_decoding(true);
    let software = MediaPool::new();
    software.add_bytes(path, bytes.clone());

    // What export jobs read is the pool's one exact view (a stable address: jobs compare
    // footage sources by address).
    let source: &dyn FootageSource = &pool;
    let exact = effectcraft_render::exact_footage(source);
    assert!(std::ptr::addr_eq(exact as *const dyn FootageSource, pool.exact() as *const MediaPool));
    assert!(std::ptr::addr_eq(effectcraft_render::exact_footage(&pool.clone()) as *const dyn FootageSource, pool.exact() as *const MediaPool));

    let before = effectcraft_platform::stats();
    for i in 0..n {
        let got = pool.exact().frame_at(&footage, at(i)).expect("exact frame");
        assert!(exact.frame_pixels(ItemId(1), &footage, at(i)).is_some());
        assert_eq!(got.data, software.frame_at(&footage, at(i)).expect("software frame").data, "frame {i}: exact");
    }
    let after = effectcraft_platform::stats();
    assert_eq!((after.decoders, after.frames), (before.decoders, before.frames), "the exact view made no hardware decoder");

    if !available {
        eprintln!("no hardware ProRes decoder: preview half skipped");
        return;
    }
    // Previews decode on the hardware: visually the same pictures.
    for i in 0..n {
        let preview = pool.frame_at(&footage, at(i)).expect("preview frame");
        let exact = software.frame_at(&footage, at(i)).expect("software frame");
        let worst = preview.data.iter().zip(&exact.data).flat_map(|(p, q)| (0..4).map(move |c| (p[c] - q[c]).abs())).fold(0f32, f32::max);
        assert!(worst < 0.01, "frame {i}: preview differs by {worst}");
    }
    let previewed = effectcraft_platform::stats();
    assert!(previewed.decoders > after.decoders && previewed.frames > after.frames, "previews used the hardware: {after:?} -> {previewed:?}");
    // … and the exact view still reads the exact frames (kept apart from the preview's).
    for i in 0..n {
        assert_eq!(pool.exact().frame_at(&footage, at(i)).expect("exact frame").data, software.frame_at(&footage, at(i)).expect("software").data, "frame {i}");
    }
    // Turned off, previews decode exactly too.
    pool.set_hardware_decoding(false);
    pool.clear_frames();
    let off = effectcraft_platform::stats();
    for i in 0..n {
        assert_eq!(pool.frame_at(&footage, at(i)).expect("frame").data, software.frame_at(&footage, at(i)).expect("software").data, "frame {i}: off");
    }
    assert_eq!(effectcraft_platform::stats().frames, off.frames, "hardware off: no hardware frames");
}
