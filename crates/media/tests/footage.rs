//! Footage import against ffmpeg as an external oracle.
//!
//! Fixtures are generated with ffmpeg into `target/fixtures/media` (never committed); every test
//! skips (passes with a note) when ffmpeg is not installed. Decoded frames are compared with
//! frames ffmpeg extracts to PNG (PSNR, premultiplied RGBA).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Instant;

use effectcraft_media::{MediaPool, probe};
use effectcraft_project::{AlphaMode, Footage, FootageKind, ItemId};
use effectcraft_raster::Image;
use effectcraft_render::FootageSource;
use effectcraft_time::{FrameRate, TICKS_PER_SECOND, Tick};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/fixtures/media")
}

fn have_ffmpeg() -> bool {
    static HAVE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *HAVE.get_or_init(|| Command::new("ffmpeg").arg("-version").output().is_ok_and(|o| o.status.success()))
}

/// Run ffmpeg to produce `name` (once; written to a temp name and renamed so concurrent test
/// processes never see a partial file). `None` when ffmpeg is unavailable.
fn ffmpeg_fixture(name: &str, args: &[&str]) -> Option<PathBuf> {
    if !have_ffmpeg() {
        eprintln!("ffmpeg not found: skipping");
        return None;
    }
    let out = fixtures().join(name);
    if out.exists() {
        return Some(out);
    }
    std::fs::create_dir_all(out.parent().expect("parent")).expect("mkdir");
    let ext = out.extension().map(|e| e.to_string_lossy().to_string()).unwrap_or_default();
    let tmp = out.with_file_name(format!("tmp-{}-{:?}.{ext}", std::process::id(), std::thread::current().id()).replace(['(', ')'], ""));
    let st = Command::new("ffmpeg").args(["-hide_banner", "-loglevel", "error", "-y"]).args(args).arg(&tmp).status().expect("run ffmpeg");
    assert!(st.success(), "ffmpeg failed for {name}");
    std::fs::rename(&tmp, &out).expect("rename");
    Some(out)
}

fn h264_mp4() -> Option<PathBuf> {
    ffmpeg_fixture(
        "testsrc2_1080p.mp4",
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=1920x1080:rate=30:duration=2",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000:duration=2",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-preset",
            "medium",
            "-crf",
            "18",
            "-colorspace",
            "bt709",
            "-color_primaries",
            "bt709",
            "-color_trc",
            "bt709",
            "-c:a",
            "aac",
            "-ac",
            "2",
            "-shortest",
        ],
    )
}

fn prores_mov() -> Option<PathBuf> {
    ffmpeg_fixture(
        "prores4444_alpha.mov",
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=640x360:rate=24:duration=1,format=rgba,geq=r='r(X,Y)':g='g(X,Y)':b='b(X,Y)':a='255*X/W'",
            "-c:v",
            "prores_ks",
            "-profile:v",
            "4444",
            "-pix_fmt",
            "yuva444p10le",
            "-colorspace",
            "bt709",
            "-color_primaries",
            "bt709",
            "-color_trc",
            "bt709",
        ],
    )
}

fn vp9_webm() -> Option<PathBuf> {
    ffmpeg_fixture(
        "testsrc2_vp9.webm",
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=640x360:rate=25:duration=1",
            "-c:v",
            "libvpx-vp9",
            "-pix_fmt",
            "yuv420p",
            "-crf",
            "20",
            "-b:v",
            "0",
            "-deadline",
            "good",
            "-cpu-used",
            "4",
            "-colorspace",
            "bt709",
            "-color_primaries",
            "bt709",
            "-color_trc",
            "bt709",
        ],
    )
}

fn png_sequence() -> Option<PathBuf> {
    // ffmpeg's image2 muxer writes seq_0001.png … seq_0010.png; generate into a temp dir and rename it.
    if !have_ffmpeg() {
        return None;
    }
    let dir = fixtures().join("seq");
    if !dir.join("seq_0010.png").exists() {
        let tmp = fixtures().join(format!("seq-tmp-{}-{:?}", std::process::id(), std::thread::current().id()).replace(['(', ')'], ""));
        std::fs::create_dir_all(&tmp).expect("mkdir");
        let st = Command::new("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i", "testsrc2=size=320x240:rate=30:duration=1", "-frames:v", "10"])
            .arg(tmp.join("seq_%04d.png"))
            .status()
            .expect("ffmpeg");
        assert!(st.success());
        if std::fs::rename(&tmp, &dir).is_err() {
            let _ = std::fs::remove_dir_all(&tmp); // another process won the race
        }
    }
    Some(dir.join("seq_0001.png"))
}

fn alpha_png() -> Option<PathBuf> {
    ffmpeg_fixture(
        "alpha.png",
        &["-f", "lavfi", "-i", "testsrc2=size=256x128:rate=1:duration=1,format=rgba,geq=r='r(X,Y)':g='g(X,Y)':b='b(X,Y)':a='255*Y/H'", "-frames:v", "1"],
    )
}

fn tone_wav() -> Option<PathBuf> {
    ffmpeg_fixture("tone.wav", &["-f", "lavfi", "-i", "sine=frequency=1000:sample_rate=48000:duration=1", "-ac", "2", "-c:a", "pcm_s16le"])
}

/// Frame `n` of `src` as RGBA PNG extracted by ffmpeg (Y'CbCr → RGB with the BT.709 matrix).
fn reference_frame(src: &Path, n: usize) -> Image {
    let stem = src.file_stem().expect("stem").to_string_lossy().to_string();
    let name = format!("ref_{stem}_{n}.png");
    let vf = format!("select='eq(n\\,{n})',scale=in_color_matrix=bt709:in_range=auto:out_range=pc:flags=bilinear+accurate_rnd+full_chroma_int,format=rgba");
    let p = ffmpeg_fixture(&name, &["-i", src.to_str().expect("utf8"), "-vf", &vf, "-fps_mode", "passthrough", "-frames:v", "1"]).expect("ffmpeg");
    load_png(&p)
}

/// A PNG as a premultiplied image (straight alpha in the file).
fn load_png(p: &Path) -> Image {
    let img = image::open(p).expect("png").to_rgba8();
    let (w, h) = img.dimensions();
    let data = img
        .pixels()
        .map(|p| {
            let a = p[3] as f32 / 255.0;
            [p[0] as f32 / 255.0 * a, p[1] as f32 / 255.0 * a, p[2] as f32 / 255.0 * a, a]
        })
        .collect();
    Image { width: w, height: h, data }
}

fn psnr(a: &Image, b: &Image) -> f64 {
    assert_eq!((a.width, a.height), (b.width, b.height), "size");
    let mut se = 0f64;
    for (p, q) in a.data.iter().zip(&b.data) {
        for k in 0..4 {
            let d = (p[k].clamp(0.0, 1.0) - q[k]) as f64;
            se += d * d;
        }
    }
    let mse = se / (a.data.len() * 4) as f64;
    if mse == 0.0 { f64::INFINITY } else { 10.0 * (1.0 / mse).log10() }
}

fn t_of(rate: FrameRate, n: i64) -> Tick {
    rate.tick_of(n)
}

#[test]
fn h264_mp4_probe_and_frames() {
    let Some(p) = h264_mp4() else { return };
    let f = probe(&p).expect("probe");
    assert_eq!(f.kind, FootageKind::Video);
    assert_eq!((f.width, f.height), (1920, 1080));
    assert_eq!(f.frame_rate, FrameRate::FPS_30);
    assert!((f.duration.seconds() - 2.0).abs() < 0.1, "duration {:?}", f.duration.seconds());
    assert!(f.has_video && f.has_audio);
    assert_eq!(f.alpha, AlphaMode::Ignore);
    assert!(!f.codec.is_empty());
    eprintln!("h264: codec {:?}", f.codec);
    let pool = MediaPool::new();
    // sequential, then a backwards seek and a forward jump into the GOP
    for n in [0usize, 1, 2, 3, 45, 10, 59] {
        let img = pool.frame(ItemId(1), &f, t_of(f.frame_rate, n as i64)).expect("frame");
        let db = psnr(&img, &reference_frame(&p, n));
        eprintln!("h264 frame {n}: {db:.2} dB");
        assert!(db > 35.0, "frame {n}: {db:.2} dB");
    }
    // clamped past the end: last frame
    let last = pool.frame_at(&f, Tick(10 * TICKS_PER_SECOND)).expect("frame");
    let db = psnr(&last, &reference_frame(&p, 59));
    assert!(db > 35.0, "clamp: {db:.2} dB");
    // cached (kept as the decoder's 4:2:0 samples, converted on use: the same pixels)
    let a = pool.frame_at(&f, t_of(f.frame_rate, 45)).expect("frame");
    let hits = pool.stats().hits;
    let b = pool.frame_at(&f, t_of(f.frame_rate, 45)).expect("frame");
    assert_eq!(pool.stats().hits, hits + 1);
    assert!(a.data == b.data);
    assert!(pool.stats().bytes <= pool.stats().frames * (1920 * 1080 * 3 / 2 + 64), "{:?}", pool.stats());
}

/// A reduced-resolution render's movie frames are converted at its size: they match resampling
/// the full frames (testsrc2's saturated colours clip, where the two differ most), and are cached
/// per size.
#[test]
fn h264_frames_at_reduced_sizes() {
    let Some(p) = h264_mp4() else { return };
    let f = probe(&p).expect("probe");
    let pool = MediaPool::new();
    for (n, (w, h)) in [(0i64, (960, 540)), (1, (640, 360)), (2, (480, 270)), (30, (323, 182))] {
        let t = t_of(f.frame_rate, n);
        let scale = w as f64 / f.width as f64;
        let preview = pool.preview_frame(ItemId(1), &f, t, f.width, f.height, scale).expect("frame");
        assert!(preview.preview, "resampled by the pool");
        let sized = preview.px.image().expect("float pixels").clone();
        let full = pool.frame(ItemId(1), &f, t).expect("frame");
        assert_eq!((sized.width, sized.height), (w, h));
        let db = psnr(&sized, &effectcraft_raster::resample(&full, w, h));
        eprintln!("h264 frame {n} at {w}×{h}: {db:.2} dB against the resampled full frame");
        // Converting at half size averages before the YUV→RGB clip, so saturated pixels differ
        // slightly; how much depends on the encoder that made the fixture (50 dB with x264 from
        // ffmpeg 6.1, above 55 with newer ones), all well past visible.
        assert!(db > 45.0, "frame {n} at {w}×{h}: {db:.2} dB");
        let again = pool.preview_frame(ItemId(1), &f, t, f.width, f.height, scale).expect("frame");
        assert!(Arc::ptr_eq(&preview.px, &again.px), "cached");
    }
}

#[test]
fn h264_audio_samples() {
    let Some(p) = h264_mp4() else { return };
    let f = probe(&p).expect("probe");
    let pool = MediaPool::new();
    let s = pool.audio_samples(&f, Tick(TICKS_PER_SECOND / 2), 4800, 48_000);
    assert_eq!(s.len(), 9600);
    let rms = (s.iter().map(|v| v * v).sum::<f32>() / s.len() as f32).sqrt();
    // ffmpeg's sine source: −24 dBFS RMS
    assert!((rms - 0.0625).abs() < 0.01, "rms {rms}");
    // before the start: silence
    let pre = pool.audio_samples(&f, Tick(-TICKS_PER_SECOND), 100, 48_000);
    assert!(pre.iter().all(|v| *v == 0.0));
}

/// H.264 + AAC at 44.1 kHz in MP4 (ffmpeg writes an edit list that skips the AAC priming):
/// 440 Hz left, a 200 → 1000 Hz sweep right, 29.97 fps.
fn aac_44k_mp4() -> Option<PathBuf> {
    ffmpeg_fixture(
        "aac_44k.mp4",
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=64x48:rate=30000/1001:duration=3",
            "-f",
            "lavfi",
            "-i",
            "aevalsrc=0.5*sin(2*PI*440*t)|0.5*sin(2*PI*(200+400*t/3)*t):s=44100:d=3",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
            "-shortest",
        ],
    )
}

/// `src`'s audio decoded by ffmpeg as interleaved stereo `f32` at `rate` Hz (external oracle).
fn ffmpeg_audio(src: &Path, rate: u32) -> Vec<f32> {
    let out = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-i"])
        .arg(src)
        .args(["-f", "f32le", "-ac", "2", "-ar", &rate.to_string(), "-"])
        .output()
        .expect("ffmpeg");
    assert!(out.status.success(), "ffmpeg failed to decode {}", src.display());
    out.stdout.as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect()
}

/// Footage audio at a rate other than the file's, read in one-frame pieces as export and the
/// preview mix do, matches ffmpeg's decode: a resampled MP4 read ramped from a wrong position
/// at the start of every piece (peaks near 100 against a 0.5 tone), so AAC footage stuttered in
/// previews and exports (#274).
#[test]
fn resampled_audio_read_in_frame_pieces_matches_ffmpeg() {
    let (Some(a), Some(b)) = (aac_44k_mp4(), h264_mp4()) else { return };
    // 44.1 kHz read at 48 kHz, 48 kHz read at 44.1 kHz; NTSC frames are fractional in samples.
    for (p, rate) in [(a, 48_000u32), (b, 44_100)] {
        let f = probe(&p).expect("probe");
        let want = ffmpeg_audio(&p, rate);
        let fps = FrameRate::new(30000, 1001);
        let pool = MediaPool::new();
        let (mut got, mut cursor) = (Vec::new(), 0i64);
        for k in 1..=55 {
            let end = fps.tick_of(k).to_units_floor(rate as i64);
            got.extend(pool.audio_samples(&f, Tick::from_units(cursor, rate as i64), (end - cursor) as usize, rate));
            cursor = end;
        }
        let worst = got.iter().zip(&want).map(|(g, w)| (g - w).abs()).fold(0.0f32, f32::max);
        eprintln!("{} at {rate} Hz: max error {worst:.4}", p.display());
        assert!(want.len() >= got.len() && worst < 0.01, "{} at {rate} Hz: max error {worst}", p.display());
    }
}

#[test]
fn prores_4444_with_alpha() {
    let Some(p) = prores_mov() else { return };
    let f = probe(&p).expect("probe");
    assert_eq!(f.kind, FootageKind::Video);
    assert_eq!((f.width, f.height), (640, 360));
    assert_eq!(f.frame_rate, FrameRate::FPS_24);
    assert!(!f.has_audio);
    assert_eq!(f.alpha, AlphaMode::Straight, "ProRes 4444 alpha detected");
    eprintln!("prores: codec {:?}", f.codec);
    let pool = MediaPool::new();
    for n in [0usize, 7, 23] {
        let img = pool.frame_at(&f, t_of(f.frame_rate, n as i64)).expect("frame");
        let db = psnr(&img, &reference_frame(&p, n));
        eprintln!("prores frame {n}: {db:.2} dB");
        assert!(db > 35.0, "frame {n}: {db:.2} dB");
    }
    // alpha ramps left to right
    let img = pool.frame_at(&f, Tick::ZERO).expect("frame");
    assert!(img.data[0][3] < 0.02 && img.data[639][3] > 0.97);
    // Ignore → opaque
    let mut g = f.clone();
    g.alpha = AlphaMode::Ignore;
    assert!(pool.frame_at(&g, Tick::ZERO).expect("frame").data.iter().all(|p| p[3] == 1.0));
}

#[test]
fn vp9_webm_frames() {
    let Some(p) = vp9_webm() else { return };
    let f = probe(&p).expect("probe");
    assert_eq!(f.kind, FootageKind::Video);
    assert_eq!((f.width, f.height), (640, 360));
    assert_eq!(f.frame_rate, FrameRate::FPS_25);
    eprintln!("vp9: codec {:?}", f.codec);
    let pool = MediaPool::new();
    for n in [0usize, 1, 12, 24, 5] {
        let img = pool.frame_at(&f, t_of(f.frame_rate, n as i64)).expect("frame");
        let db = psnr(&img, &reference_frame(&p, n));
        eprintln!("vp9 frame {n}: {db:.2} dB");
        assert!(db > 35.0, "frame {n}: {db:.2} dB");
    }
}

#[test]
fn png_sequence_import() {
    let Some(first) = png_sequence() else { return };
    // One file probes as a still; the engine groups numbered files into the sequence.
    let mut f = probe(&first).expect("probe");
    assert_eq!(f.kind, FootageKind::Still);
    f.kind = FootageKind::Sequence;
    f.sequence = (1..=10).map(|n| first.with_file_name(format!("seq_{n:04}.png")).to_string_lossy().to_string()).collect();
    f.sync_sequence_duration();
    assert_eq!((f.width, f.height), (320, 240));
    assert_eq!(f.duration, FrameRate::FPS_30.tick_of(10));
    let pool = MediaPool::new();
    let img = pool.frame_at(&f, t_of(f.frame_rate, 3)).expect("frame");
    assert_eq!(*img, load_png(&first.with_file_name("seq_0004.png")));
    // past the end clamps to the last file; with 2 loops it wraps
    let img = pool.frame_at(&f, t_of(f.frame_rate, 13)).expect("frame");
    assert_eq!(*img, load_png(&first.with_file_name("seq_0010.png")));
    let mut looped = f.clone();
    looped.loop_count = 2;
    let img = pool.frame_at(&looped, t_of(f.frame_rate, 13)).expect("frame");
    assert_eq!(*img, load_png(&first.with_file_name("seq_0004.png")));
}

/// A gap in a sequence's numbering shows colour bars, as in After Effects; in Force
/// Alphabetical Order the files play back to back (#297).
#[test]
fn sequence_gaps_show_placeholders() {
    let dir = fixtures().join(format!("gap-seq-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("mkdir");
    // Frames 1, 2 and 4: frame 3 is missing. Each frame is a flat grey of its number.
    let grey = |n: u8| image::RgbaImage::from_pixel(8, 4, image::Rgba([n * 50, n * 50, n * 50, 255]));
    let mut files = vec![];
    for n in [1u8, 2, 4] {
        let p = dir.join(format!("gap_{n:03}.png"));
        grey(n).save(&p).expect("write png");
        files.push(p.to_string_lossy().to_string());
    }
    let mut f = Footage { kind: FootageKind::Sequence, sequence: files, ..probe(dir.join("gap_001.png")).expect("probe") };
    f.sync_sequence_duration();
    assert_eq!(f.duration, f.frame_rate.tick_of(4), "frames 1-4");
    let pool = MediaPool::new();
    let at = |f: &Footage, n: i64| pool.frame_at(f, t_of(f.frame_rate, n)).expect("frame");
    let level = |img: &Image| (img.get(0, 0)[0] * 255.0).round() as u32;
    assert_eq!(level(&at(&f, 1)), 100);
    // The placeholder: colour bars across the frame, starting with 75 % white.
    let bars = at(&f, 2);
    assert_eq!((bars.width, bars.height), (8, 4));
    assert_eq!(bars.get(0, 0), [0.75, 0.75, 0.75, 1.0]);
    assert_eq!(bars.get(7, 3), [0.0, 0.0, 0.75, 1.0], "blue on the right");
    assert_eq!(level(&at(&f, 3)), 200);
    f.alphabetical = true;
    f.sync_sequence_duration();
    assert_eq!(f.duration, f.frame_rate.tick_of(3));
    assert_eq!(level(&at(&f, 2)), 200, "frame 4 follows frame 2");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn png_with_alpha() {
    let Some(p) = alpha_png() else { return };
    let f = probe(&p).expect("probe");
    assert_eq!(f.kind, FootageKind::Still);
    assert_eq!((f.width, f.height), (256, 128));
    assert_eq!(f.alpha, AlphaMode::Straight);
    assert_eq!(f.codec, "PNG");
    let pool = MediaPool::new();
    let a = pool.frame_at(&f, Tick::ZERO).expect("frame");
    let b = pool.frame_at(&f, Tick(5 * TICKS_PER_SECOND)).expect("frame");
    let st = pool.stats();
    assert!(st.frames == 1 && st.hits == 1 && a.data == b.data, "stills are timeless: {st:?}");
    assert_eq!(st.bytes, 256 * 128 * 4 + 64, "kept as 8-bit RGBA");
    assert_eq!(*a, load_png(&p));
    assert!(a.data[0][3] == 0.0 && a.data[a.data.len() - 1][3] > 0.98);
    let th = pool.thumbnail(&f, 64).expect("thumb");
    assert_eq!((th.width, th.height), (64, 32));
}

#[test]
fn wav_audio() {
    let Some(p) = tone_wav() else { return };
    let f = probe(&p).expect("probe");
    assert_eq!(f.kind, FootageKind::Audio);
    assert!(f.has_audio && !f.has_video);
    assert!((f.duration.seconds() - 1.0).abs() < 0.01);
    let pool = MediaPool::new();
    assert!(pool.frame(ItemId(1), &f, Tick::ZERO).is_none());
    let s = pool.audio_samples(&f, Tick::ZERO, 48_000, 48_000);
    let rms = (s.iter().map(|v| v * v).sum::<f32>() / s.len() as f32).sqrt();
    assert!((rms - 0.0625).abs() < 0.005, "rms {rms}");
    // resampled to 44.1 kHz: same loudness
    let s = pool.audio_samples(&f, Tick::ZERO, 44_100, 44_100);
    let rms = (s.iter().map(|v| v * v).sum::<f32>() / s.len() as f32).sqrt();
    assert!((rms - 0.0625).abs() < 0.01, "rms {rms}");
}

#[test]
fn cache_budget_evicts() {
    let Some(p) = vp9_webm() else { return };
    let f = probe(&p).expect("probe");
    // 8-bit 4:2:0 frames are cached as their samples: 1.5 bytes a pixel.
    let frame_bytes = 640 * 360 * 3 / 2 + 64;
    let pool = MediaPool::with_budget(3 * frame_bytes + 1024);
    for n in 0..10 {
        pool.frame_at(&f, t_of(f.frame_rate, n)).expect("frame");
    }
    let st = pool.stats();
    assert!(st.frames <= 3 && st.bytes <= 3 * frame_bytes + 1024, "{st:?}");
    pool.set_budget(0);
    assert_eq!(pool.stats().frames, 1);
}

#[test]
fn concurrent_requests() {
    let Some(p) = h264_mp4() else { return };
    let f = probe(&p).expect("probe");
    let pool = MediaPool::new();
    std::thread::scope(|s| {
        for k in 0..4i64 {
            let (pool, f) = (&pool, &f);
            s.spawn(move || {
                for n in 0..15 {
                    let img = pool.frame_at(f, t_of(f.frame_rate, k * 15 + n)).expect("frame");
                    assert_eq!((img.width, img.height), (1920, 1080));
                }
            });
        }
    });
}

#[test]
fn perf_sequential_1080p_h264() {
    let Some(p) = h264_mp4() else { return };
    let f = probe(&p).expect("probe");
    let n = 60;
    // FilmCraft alone (decode only, YUV frames)
    let bytes: Arc<[u8]> = std::fs::read(&p).expect("read").into();
    let src = filmcraft_codecs::open_bytes("a.mp4", bytes).expect("open");
    let t0 = Instant::now();
    for i in 0..n {
        let t = filmcraft_time::Tick(f.frame_rate.tick_of(i).0 + f.frame_rate.frame_duration().0 / 2);
        src.video_frame(filmcraft_media::FrameRequest::full(t)).expect("frame");
    }
    let raw = n as f64 / t0.elapsed().as_secs_f64();
    // Through the pool (decode + conversion to f32 RGBA), cold cache, without and with read-ahead
    let pool = MediaPool::new();
    pool.set_read_ahead(false);
    let t0 = Instant::now();
    for i in 0..n {
        pool.frame_at(&f, t_of(f.frame_rate, i)).expect("frame");
    }
    let plain = n as f64 / t0.elapsed().as_secs_f64();
    let pool = MediaPool::new();
    let t0 = Instant::now();
    for i in 0..n {
        pool.frame_at(&f, t_of(f.frame_rate, i)).expect("frame");
    }
    let ours = n as f64 / t0.elapsed().as_secs_f64();
    assert!(pool.stats().prefetched > 0);
    // Cached (the last 20 frames fit the default budget)
    let t0 = Instant::now();
    for i in n - 20..n {
        pool.frame_at(&f, t_of(f.frame_rate, i)).expect("frame");
    }
    let cached = 20.0 / t0.elapsed().as_secs_f64();
    assert!(pool.stats().hits >= 20);
    eprintln!(
        "1080p H.264 sequential: filmcraft decode {raw:.1} fps; MediaPool (decode + RGBA f32) {plain:.1} fps, with read-ahead {ours:.1} fps; cached {cached:.0} fps"
    );
}

/// Rayon workers asking for a frame another worker is decoding wait for it instead of each
/// decoding a private copy: one decode, the same pixels for everyone.
#[test]
fn rayon_workers_wait_for_an_in_flight_frame() {
    use rayon::prelude::*;
    let Some(p) = h264_mp4() else { return };
    let f = probe(&p).expect("probe");
    let pool = MediaPool::new();
    let workers = rayon::ThreadPoolBuilder::new().num_threads(10).build().expect("pool");
    // Read frame 39 first, so the shared decode of frame 40 is one step of an open decoder: a
    // decode from the keyframe can outlast the workers' stall timeout when the machine is busy
    // (the full suite), and then they decode private copies.
    pool.frame_at(&f, t_of(f.frame_rate, 39)).expect("frame 39");
    let before = pool.stats();
    let t = t_of(f.frame_rate, 40);
    let frames: Vec<Arc<Image>> = workers.install(|| (0..10).into_par_iter().with_max_len(1).map(|_| pool.frame_at(&f, t).expect("frame")).collect());
    let st = pool.stats();
    assert_eq!((st.misses - before.misses, st.hits - before.hits), (1, 9), "{st:?}");
    assert!(frames.iter().all(|img| img.data == frames[0].data));
    let db = psnr(&frames[0], &reference_frame(&p, 40));
    assert!(db > 35.0, "frame 40: {db:.2} dB");
}

/// Parallel export reads footage frames from many rayon workers at once, in roughly sequential
/// order: the pool decodes each frame once (in-flight frames are waited for, not decoded again;
/// the decode-ahead runs ahead of the workers) and every frame comes out exactly as a plain
/// sequential read decodes it.
#[test]
fn parallel_sequential_reads_match_sequential_decoding() {
    use rayon::prelude::*;
    let Some(p) = h264_mp4() else { return };
    let f = probe(&p).expect("probe");
    let n = 60;
    let plain = MediaPool::new();
    plain.set_read_ahead(false);
    let reference: Vec<Arc<Image>> = (0..n).map(|i| plain.frame_at(&f, t_of(f.frame_rate, i)).expect("frame")).collect();
    let workers = rayon::ThreadPoolBuilder::new().num_threads(10).build().expect("pool");
    let mut fps = [Vec::new(), Vec::new()];
    for _ in 0..3 {
        for (k, read_ahead) in [false, true].into_iter().enumerate() {
            let pool = MediaPool::new();
            pool.set_read_ahead(read_ahead);
            let (g0, t0) = (filmcraft_codecs::gop_stats(), Instant::now());
            let frames: Vec<Arc<Image>> = workers.install(|| {
                (0..n as i32).into_par_iter().with_max_len(1).map(|i| pool.frame_at(&f, t_of(f.frame_rate, i64::from(i))).expect("frame")).collect()
            });
            fps[k].push(n as f64 / t0.elapsed().as_secs_f64());
            let g = filmcraft_codecs::gop_stats() - g0;
            for (i, (a, b)) in frames.iter().zip(&reference).enumerate() {
                assert!(a.data == b.data, "frame {i} (read-ahead {read_ahead}) differs from a sequential decode");
            }
            eprintln!("parallel read (read-ahead {read_ahead}): {:?}; samples decoded {}, seeks {}", pool.stats(), g.decoded, g.seeks);
        }
    }
    let median = |v: &mut Vec<f64>| {
        v.sort_by(f64::total_cmp);
        v[v.len() / 2]
    };
    let (off, on) = (median(&mut fps[0]), median(&mut fps[1]));
    eprintln!("1080p H.264, 60 frames on 10 rayon workers (median of 3): {off:.1} fps without decode-ahead, {on:.1} fps with");
    // Export-like: each worker renders for a while (a sleep: no CPU taken from the decoder) with
    // the frame's samples, as the compositor does.
    let mut fps = [Vec::new(), Vec::new()];
    for _ in 0..3 {
        for (k, read_ahead) in [false, true].into_iter().enumerate() {
            let pool = MediaPool::new();
            pool.set_read_ahead(read_ahead);
            let t0 = Instant::now();
            workers.install(|| {
                (0..n as i32).into_par_iter().with_max_len(1).for_each(|i| {
                    pool.frame_pixels(ItemId(1), &f, t_of(f.frame_rate, i64::from(i))).expect("frame");
                    std::thread::sleep(std::time::Duration::from_millis(40));
                })
            });
            fps[k].push(n as f64 / t0.elapsed().as_secs_f64());
        }
    }
    let (off, on) = (median(&mut fps[0]), median(&mut fps[1]));
    eprintln!("… with 40 ms of render per frame (median of 3): {off:.1} fps without decode-ahead, {on:.1} fps with");
}

#[test]
fn exr_still_is_linear_float() {
    // written with the image crate's OpenEXR encoder (no ffmpeg needed)
    let dir = fixtures();
    std::fs::create_dir_all(&dir).expect("mkdir");
    let p = dir.join(format!("float-{}.exr", std::process::id()));
    // linear 0.214 ≈ sRGB 0.5 (opaque); premultiplied 2.0 at alpha 0.5
    let px = vec![0.214f32, 0.214, 0.214, 1.0, 2.0, 1.0, 0.5, 0.5];
    image::DynamicImage::ImageRgba32F(image::ImageBuffer::from_raw(2, 1, px).expect("buf")).save(&p).expect("write exr");
    let f = probe(&p).expect("probe");
    assert_eq!(f.kind, FootageKind::Still);
    assert_eq!((f.width, f.height), (2, 1));
    assert_eq!(f.codec, "EXR");
    assert_eq!(f.alpha, AlphaMode::Premultiplied);
    let img = MediaPool::new().frame_at(&f, Tick::ZERO).expect("frame");
    std::fs::remove_file(&p).ok();
    let a = img.data[0];
    assert!((a[0] - 0.5).abs() < 2e-3 && a[3] == 1.0, "{a:?}");
    // straight 4.0 → encoded ≈ 1.83 (super-white kept) → re-premultiplied ≈ 0.92
    let b = img.data[1];
    assert!(b[3] == 0.5 && b[0] > 0.85 && b[0] < 1.0, "{b:?}");
}

/// A FLAC of `secs` seconds at `rate` Hz with `channels` channels of a 440 Hz sine (first two:
/// −24 dBFS RMS, the rest quieter), samples of `fmt` (`s16`, or `s32` with 24 significant bits).
fn sine_flac(secs: u32, rate: u32, channels: u32, fmt: &str) -> Option<PathBuf> {
    let name = format!("sine_{secs}s_{rate}_{channels}ch_{fmt}.flac");
    let src = format!("sine=frequency=440:sample_rate={rate}:duration={secs}");
    let pan = format!("pan={channels}c|{}", (0..channels).map(|c| format!("c{c}={}*c0", if c < 2 { "1" } else { "0.5" })).collect::<Vec<_>>().join("|"));
    let mut args = vec!["-f", "lavfi", "-i", &src, "-af", &pan, "-sample_fmt", fmt];
    if fmt == "s32" {
        args.extend(["-bits_per_raw_sample", "24"]);
    }
    args.extend(["-c:a", "flac"]);
    ffmpeg_fixture(&name, &args)
}

/// #408 robustness: FLAC footage of any rate, depth and channel count reads correctly as the
/// preview feeder reads it, one device block at a time at the mix rate, through its end and past
/// it: two minutes of 24-bit, 6-channel, 96 kHz audio, and mono 8 kHz, 11.025 kHz, 192 kHz and
/// 8-channel files.
#[test]
fn flac_audio_of_any_shape_reads_in_device_blocks() {
    for (secs, rate, channels, fmt) in [(120, 96_000, 6, "s32"), (5, 8_000, 1, "s16"), (5, 11_025, 2, "s16"), (5, 192_000, 2, "s32"), (5, 48_000, 8, "s16")] {
        let Some(p) = sine_flac(secs, rate, channels, fmt) else { return };
        let f = probe(&p).expect("probe");
        assert!(f.has_audio && f.kind == FootageKind::Audio, "{}", p.display());
        assert!((f.duration.seconds() - f64::from(secs)).abs() < 0.01, "{}: {}", p.display(), f.duration.seconds());
        let pool = MediaPool::new();
        let mix = 48_000i64;
        let total = f.duration.to_units_floor(mix);
        let (mut at, mut sum, mut n, mut worst) = (0i64, 0.0f64, 0usize, std::time::Duration::ZERO);
        let t0 = Instant::now();
        while at < total + 4096 {
            let t = Instant::now();
            let s = pool.audio_samples(&f, Tick::from_units(at, mix), 1024, 48_000);
            if at > 0 {
                worst = worst.max(t.elapsed());
            }
            assert_eq!(s.len(), 2048);
            assert!(s.iter().all(|v| v.is_finite() && v.abs() <= 1.0), "{} at {at}", p.display());
            if at + 1024 <= total {
                sum += s.iter().map(|v| f64::from(*v) * f64::from(*v)).sum::<f64>();
                n += s.len();
            } else if at >= total {
                assert!(s.iter().all(|v| *v == 0.0), "{}: silence past the end", p.display());
            }
            at += 1024;
        }
        // ffmpeg's sine source has an amplitude of 1/8, on both channels (mono is duplicated).
        let rms = (sum / n.max(1) as f64).sqrt();
        assert!((rms - 0.125 / 2f64.sqrt()).abs() < 0.003, "{}: rms {rms}", p.display());
        eprintln!("{}: read {secs} s in {:?}, slowest block after the first {worst:?}", p.display(), t0.elapsed());
        assert_eq!(pool.stats().opened, 1, "opened once, not per block");
    }
}

/// #408: the first reads of a FLAC from many threads at once (a preview's frame workers, audio
/// feeder, conform and waveform threads) decode it once: each decoded the whole file, so memory
/// and time grew with the number of threads.
#[test]
fn concurrent_first_reads_open_a_file_once() {
    let Some(p) = sine_flac(60, 44_100, 2, "s16") else { return };
    let f = probe(&p).expect("probe");
    let pool = MediaPool::new();
    let start = Arc::new(std::sync::Barrier::new(8));
    let reads: Vec<_> = (0..8)
        .map(|_| {
            let (pool, f, start) = (pool.clone(), f.clone(), start.clone());
            std::thread::spawn(move || {
                start.wait();
                pool.audio_samples(&f, Tick(TICKS_PER_SECOND), 1024, 48_000)
            })
        })
        .collect();
    let reads: Vec<Vec<f32>> = reads.into_iter().map(|t| t.join().expect("read")).collect();
    assert_eq!(pool.stats().opened, 1, "the file was opened once");
    assert!(reads.iter().all(|r| *r == reads[0]) && reads[0].iter().any(|v| *v != 0.0));
}

/// Settings ▸ Disk ▸ Conformed Audio Folder: a FLAC's conformed file, written a chunk at a time,
/// reads back exactly as the decoder reads.
#[test]
fn conformed_flac_audio_reads_back_like_the_decoder() {
    let Some(p) = sine_flac(5, 44_100, 2, "s16") else { return };
    let f = probe(&p).expect("probe");
    let folder = fixtures().join(format!("conformed-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&folder);
    let pool = MediaPool::new();
    let at = Tick::from_seconds_f64(2.5);
    let decoded = pool.audio_samples(&f, at, 4800, 48_000);
    pool.set_conform_folder(Some(folder.clone()));
    let file = pool.conformed_path(&p.to_string_lossy(), 48_000).expect("a conformed path");
    pool.audio_samples(&f, at, 4800, 48_000);
    let t0 = Instant::now();
    while !file.exists() {
        assert!(t0.elapsed().as_secs() < 60, "the conformed file was never written");
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    // (Renamed into place once complete.) Raw interleaved stereo f32 at 48 kHz.
    let bytes = std::fs::read(&file).expect("conformed file");
    assert_eq!(bytes.len(), 5 * 48_000 * 8);
    let s0 = at.to_units_floor(48_000) as usize * 8;
    let conformed: Vec<f32> = bytes[s0..s0 + 4800 * 8].as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect();
    assert_eq!(conformed, decoded);
    assert_eq!(pool.audio_samples(&f, at, 4800, 48_000), decoded);
    let _ = std::fs::remove_dir_all(&folder);
}

/// Probing reads standalone audio from its headers and packet framing (no decode) and movies
/// from their index (through the file): it describes them exactly as opening the whole file in
/// memory does.
#[test]
fn header_probe_matches_a_whole_file_probe() {
    let Some(flac) = sine_flac(5, 44_100, 2, "s16") else { return };
    let mp3 = ffmpeg_fixture("sine_3s.mp3", &["-f", "lavfi", "-i", "sine=frequency=440:sample_rate=44100:duration=3", "-c:a", "libmp3lame", "-b:a", "128k"]);
    let mp3_cbr_no_tag = ffmpeg_fixture(
        "sine_2s_notag.mp3",
        &["-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000:duration=2", "-c:a", "libmp3lame", "-write_xing", "0"],
    );
    let files = [Some(flac), mp3, mp3_cbr_no_tag, h264_mp4(), vp9_webm(), tone_wav(), alpha_png()];
    for p in files.into_iter().flatten() {
        let fast = probe(&p).expect("probe");
        let bytes: Arc<[u8]> = std::fs::read(&p).expect("read").into();
        let whole = effectcraft_media::probe_bytes(&p.to_string_lossy(), bytes).expect("probe bytes");
        assert_eq!(fast, whole, "{}", p.display());
    }
}

/// Footage read only through [`FootageSource::frame`] (float frames at full size): the
/// renderer's reference path, before frames were kept packed and resampled by the pool.
struct FloatFrames(MediaPool);

impl FootageSource for FloatFrames {
    fn frame(&self, item: ItemId, f: &Footage, t: Tick) -> Option<Arc<Image>> {
        self.0.frame(item, f, t)
    }
}

/// A comp of `f`'s size holding one layer of it, colour-managed or not.
fn footage_comp(f: &Footage, managed: bool) -> (effectcraft_project::Project, ItemId) {
    use effectcraft_project::{BitDepth, Comp, ItemKind, LayerSource, Project, build};
    let mut p = Project::default();
    p.settings.bit_depth = BitDepth::Bpc32;
    if managed {
        p.settings.working_space = Some(effectcraft_project::ColorSpace::DisplayP3);
    }
    let comp = Comp::new(f.width, f.height, FrameRate::FPS_30, Tick::from_seconds_f64(1.0));
    let cid = p.add_item("Comp", effectcraft_color::Label::default(), None, ItemKind::Comp(comp.clone().into()));
    let fid = p.add_item("clip", effectcraft_color::Label::default(), None, ItemKind::Footage(f.clone()));
    let l = build::layer(&mut p, &comp, "clip", LayerSource::Footage { item: fid }, (f.width, f.height), None);
    p.comp_mut(cid).expect("comp").layers.push(l);
    (p, cid)
}

/// Footage layers render the same pixels whether their frames come packed (converted straight
/// into the layer buffer with the colour conversion) and, at reduced resolution, already
/// resampled by the pool, or as full-size float frames the renderer copies and resamples.
#[test]
fn packed_and_preview_frames_render_like_float_frames() {
    use effectcraft_render::{RenderOpts, Renderer};
    let (Some(movie), Some(still)) = (h264_mp4(), alpha_png()) else { return };
    for path in [movie, still] {
        let f = probe(&path).expect("probe");
        for managed in [false, true] {
            let (p, cid) = footage_comp(&f, managed);
            for scale in [1.0, 0.5, 1.0 / 3.0, 0.25] {
                let pool = MediaPool::new();
                let reference = FloatFrames(MediaPool::new());
                let opts = RenderOpts { scale, ..RenderOpts::default() };
                for k in [0i64, 1] {
                    let t = FrameRate::FPS_30.tick_of(k);
                    let a = Renderer::new(&p, &pool, opts).comp_frame(cid, t);
                    let b = Renderer::new(&p, &reference, opts).comp_frame(cid, t);
                    assert_eq!((a.width, a.height), (b.width, b.height));
                    let same = a.data.iter().zip(&b.data).all(|(x, y)| x.map(f32::to_bits) == y.map(f32::to_bits));
                    assert!(same, "{} managed {managed} scale {scale} frame {k}", path.display());
                }
                let st = pool.stats();
                if scale < 0.75 {
                    // only the preview-size frames are kept
                    let (w, h) = (((f.width as f64 * scale).round() as usize), ((f.height as f64 * scale).round() as usize));
                    assert!(st.frames > 0 && st.bytes == st.frames * (w * h * 16 + 64), "{} scale {scale}: {st:?}", path.display());
                }
            }
        }
    }
}

/// Media Browser thumbnails: the first frame is decoded without being cached and the movie's
/// file isn't kept open (browsing a folder of movies held every one of them).
#[test]
fn thumbnails_leave_no_frames_or_files_behind() {
    let (Some(movie), Some(still)) = (h264_mp4(), alpha_png()) else { return };
    let pool = MediaPool::new();
    for p in [movie, still] {
        let f = probe(&p).expect("probe");
        let th = FootageSource::thumbnail(&pool, &f, 128).expect("thumbnail");
        assert_eq!(th.width.max(th.height), 128, "{}", p.display());
        assert_eq!(pool.stats().frames, 0, "{}: no frame cached", p.display());
        assert_eq!(pool.open_files(), 0, "{}: no file kept open", p.display());
    }
}

/// #329: a frame read that is called off (a scrub has moved on) gives up before the decoder
/// seeks and decodes forward from the keyframe, so it no longer holds the decoder the newest
/// frame waits for; the frame decodes as usual when it is wanted again.
#[test]
fn a_called_off_read_does_not_decode() {
    use std::sync::atomic::AtomicBool;
    let Some(p) = h264_mp4() else { return };
    let f = probe(&p).expect("probe");
    let pool = MediaPool::new();
    let t = f.frame_rate.tick_of(45) + Tick(f.frame_rate.frame_duration().0 / 2);
    let off = Arc::new(AtomicBool::new(true));
    let mut got = None;
    pool.cancellable(&off, &mut || got = Some(pool.frame(ItemId(1), &f, t)));
    assert!(got.expect("ran").is_none(), "a called-off read decoded its frame");
    assert_eq!(pool.stats().frames, 0, "nothing cached for it");
    let on = Arc::new(AtomicBool::new(false));
    let mut got = None;
    pool.cancellable(&on, &mut || got = pool.frame(ItemId(1), &f, t));
    let img = got.expect("decoded once wanted again");
    assert_eq!((img.width, img.height), (1920, 1080));
}

/// #886: H.264 High 4:2:2 10-bit: the software decoder lacks it; the pool says why, once, and
/// the footage reports it (not a black frame without a word).
#[test]
fn an_unsupported_stream_reports_why_it_shows_no_picture() {
    let Some(p) = ffmpeg_fixture(
        "h264_high422_10bit.mp4",
        &["-f", "lavfi", "-i", "testsrc=size=320x180:rate=24:duration=1", "-c:v", "libx264", "-profile:v", "high422", "-pix_fmt", "yuv422p10le"],
    ) else {
        return;
    };
    let f = probe(&p).expect("probe");
    let pool = MediaPool::new();
    assert_eq!(pool.problem(&f), None, "nothing is wrong before a frame was asked for");
    if pool.frame(ItemId(1), &f, Tick::ZERO).is_some() {
        // a decoder that handles 4:2:2 (FilmCraft's, or a hardware one): no notice
        assert_eq!(pool.problem(&f), None);
        return;
    }
    let why = pool.problem(&f).expect("a failed frame leaves a reason");
    assert!(why.contains("4:2:2"), "names the stream: {why}");
    assert!(why.contains("isn't supported"), "says it is unsupported: {why}");
    assert!(!why.contains("decode error"), "no error wrappers: {why}");
    // it stays for the other frames, and is dropped when the file is forgotten
    assert!(pool.frame(ItemId(1), &f, Tick::from_seconds_f64(0.5)).is_none());
    assert_eq!(pool.problem(&f).as_deref(), Some(why.as_str()));
    pool.forget(&f.path);
    assert_eq!(pool.problem(&f), None);
}
