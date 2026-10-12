//! The VideoToolbox ProRes decoder against FilmCraft's software decoder (macOS with a hardware
//! ProRes decoder; elsewhere the tests note why and pass).

#![cfg(target_os = "macos")]

mod common;

use effectcraft_platform::prores::VtProResDecoder;
use filmcraft_codecs::VideoDecoder;
use filmcraft_codecs::video::ProResDecoder;
use filmcraft_prores::{ChromaFormat, Profile};

/// The samples of an ffmpeg (`prores_ks`) ProRes 4444 + alpha movie (generated once into
/// `target/fixtures/platform`, never committed); `None` without ffmpeg.
fn ffmpeg_4444_alpha() -> Option<Vec<Vec<u8>>> {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/fixtures/platform");
    let path = dir.join("prores_ks_4444_alpha_320x180.mov");
    if !path.exists() {
        std::fs::create_dir_all(&dir).ok()?;
        let tmp = dir.join(format!("tmp-{}.mov", std::process::id()));
        let st = std::process::Command::new("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i"])
            .arg("testsrc2=size=320x180:rate=24:duration=0.25,format=rgba,geq=r='r(X,Y)':g='g(X,Y)':b='b(X,Y)':a='255*X/W'")
            .args(["-c:v", "prores_ks", "-profile:v", "4444", "-pix_fmt", "yuva444p10le"])
            .arg(&tmp)
            .status()
            .ok()?;
        assert!(st.success(), "ffmpeg failed");
        std::fs::rename(&tmp, &path).expect("rename");
    }
    let file = std::fs::File::open(&path).expect("open fixture");
    let mp4 = filmcraft_isobmff::open(&file).expect("parse fixture");
    let ti = mp4.track_of_kind(filmcraft_isobmff::TrackKind::Video).expect("video track");
    Some((0..mp4.tracks[ti].samples.len()).map(|i| mp4.read_sample(&file, ti, i).expect("sample")).collect())
}

fn decode(d: &mut dyn VideoDecoder, sample: &[u8]) -> filmcraft_frame::VideoFrame {
    let mut out = d.decode(sample, 7).expect("decode");
    assert_eq!(out.len(), 1, "one picture per sample");
    let f = out.remove(0);
    assert_eq!(f.pts, 7);
    f.frame
}

/// One test (the counters are process-wide): hardware pictures are visually identical to the
/// software decoder's (Y/Cb/Cr within 4 codes at 12 bits, mean within 0.5; alpha exact; same
/// layout and colour), and a decoder whose hardware fails gives the software pictures exactly.
#[test]
fn hardware_pictures_match_software_and_failures_fall_back() {
    let cases = [
        ("422 HQ", Profile::Hq, *b"apch", ChromaFormat::Yuv422, 10, false),
        ("422 LT", Profile::Lt, *b"apcs", ChromaFormat::Yuv422, 10, false),
        ("4444", Profile::P4444, *b"ap4h", ChromaFormat::Yuv444, 12, false),
    ];
    let (w, h) = (320, 180);
    for (name, profile, fourcc, chroma, depth, alpha) in cases {
        let mut hw = match VtProResDecoder::new(fourcc, w, h) {
            Ok(d) => d,
            Err(why) => {
                eprintln!("{name}: no hardware ProRes decoder ({why}): skipping");
                continue;
            }
        };
        let mut sw = ProResDecoder;
        for seed in 0..3 {
            let sample = common::encode(profile, &common::picture(w, h, chroma, depth, alpha, seed));
            let before = effectcraft_platform::stats();
            let (a, b) = (decode(&mut sw, &sample), decode(&mut hw, &sample));
            let after = effectcraft_platform::stats();
            assert_eq!((after.frames - before.frames, after.fallbacks - before.fallbacks), (1, 0), "{name}: decoded on the hardware");
            let (planes, a_diff) = common::compare(&a, &b);
            for (plane, (max, mean)) in ["Y", "Cb", "Cr"].iter().zip(planes) {
                assert!(max <= 4 && mean <= 0.5, "{name} {plane}: max {max}, mean {mean:.3} (12-bit codes)");
            }
            if alpha {
                assert_eq!(a_diff.map(|d| d.0), Some(0), "{name}: alpha exact");
            }
        }
        // A lost session / failing hardware: the software decoder's pictures, exactly.
        hw.fail_hardware();
        let sample = common::encode(profile, &common::picture(w, h, chroma, depth, alpha, 9));
        let before = effectcraft_platform::stats();
        let (a, b) = (decode(&mut sw, &sample), decode(&mut hw, &sample));
        assert_eq!(effectcraft_platform::stats().fallbacks - before.fallbacks, 1, "{name}: counted as a fallback");
        let (planes, a_diff) = common::compare(&a, &b);
        assert!(planes.iter().all(|d| d.0 == 0) && a_diff.is_none_or(|d| d.0 == 0), "{name}: fallback pictures are the software decoder's");
        // a damaged sample is the software decoder's error, not a panic
        let mut broken = sample.clone();
        broken.truncate(40);
        assert!(hw.decode(&broken, 0).is_err() && sw.decode(&broken, 0).is_err(), "{name}: damaged sample");
    }
    // 4444 + alpha. VideoToolbox rejects the alpha FilmCraft's encoder writes (decode error
    // -12911, 12 Oct 2026), so it is generated with ffmpeg (an external fixture generator).
    match ffmpeg_4444_alpha() {
        None => eprintln!("4444 + alpha: ffmpeg not found: skipping"),
        Some(samples) => {
            if let Ok(mut hw) = VtProResDecoder::new(*b"ap4h", w, h) {
                let before = effectcraft_platform::stats();
                for sample in &samples {
                    let (a, b) = (decode(&mut ProResDecoder, sample), decode(&mut hw, sample));
                    let (planes, a_diff) = common::compare(&a, &b);
                    for (plane, (max, mean)) in ["Y", "Cb", "Cr"].iter().zip(planes) {
                        assert!(max <= 4 && mean <= 0.5, "4444 + alpha {plane}: max {max}, mean {mean:.3} (12-bit codes)");
                    }
                    assert_eq!(a_diff.map(|d| d.0), Some(0), "4444 + alpha: alpha exact");
                }
                let after = effectcraft_platform::stats();
                assert_eq!((after.frames - before.frames, after.fallbacks - before.fallbacks), (samples.len() as u64, 0), "4444 + alpha on the hardware");
            }
        }
    }
    // FilmCraft-encoded 4444 + alpha: whatever the hardware makes of it, the pictures are the
    // software decoder's (exactly, when the hardware rejects the stream) or visually identical.
    if let Ok(mut hw) = VtProResDecoder::new(*b"ap4h", w, h) {
        for seed in 0..5 {
            let sample = common::encode(Profile::P4444, &common::picture(w, h, ChromaFormat::Yuv444, 12, true, seed));
            let before = effectcraft_platform::stats();
            let (a, b) = (decode(&mut ProResDecoder, &sample), decode(&mut hw, &sample));
            let (planes, a_diff) = common::compare(&a, &b);
            let exact = planes.iter().all(|d| d.0 == 0) && a_diff.is_none_or(|d| d.0 == 0);
            let fell_back = effectcraft_platform::stats().fallbacks > before.fallbacks;
            assert!(if fell_back { exact } else { planes.iter().all(|d| d.0 <= 4) && a_diff.is_some_and(|d| d.0 == 0) }, "seed {seed}: {planes:?} {a_diff:?}");
        }
    }
    // the factory refuses what isn't ProRes or is too large
    assert!(VtProResDecoder::new(*b"avc1", w, h).is_err());
    assert!(VtProResDecoder::new(*b"ap4h", 9000, 100).is_err());
}
