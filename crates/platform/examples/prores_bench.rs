//! Times the VideoToolbox ProRes decoder against FilmCraft's software decoder on a movie, and
//! compares their pictures.
//!
//! `cargo run --release -p effectcraft-platform --example prores_bench -- <file.mov> [frames] [threads] [sw|hw|both]`

#[cfg(target_os = "macos")]
fn main() {
    use effectcraft_platform::prores::VtProResDecoder;
    use filmcraft_codecs::VideoDecoder;
    use filmcraft_codecs::video::ProResDecoder;
    use std::time::Instant;

    let args: Vec<String> = std::env::args().collect();
    let path = args.get(1).expect("usage: prores_bench <file.mov> [frames] [threads] [sw|hw|both]");
    let frames: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(120);
    let threads: usize = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(1);
    let mode = args.get(4).map(String::as_str).unwrap_or("both");
    let file = std::fs::File::open(path).expect("open");
    let mp4 = filmcraft_isobmff::open(&file).expect("parse");
    let ti = mp4.track_of_kind(filmcraft_isobmff::TrackKind::Video).expect("video track");
    let track = &mp4.tracks[ti];
    let entry = &track.entries[0];
    let filmcraft_isobmff::CodecConfig::ProRes { fourcc } = &entry.codec else { panic!("not ProRes") };
    let v = entry.video.as_ref().expect("video params");
    let n = frames.min(track.samples.len());
    let samples: Vec<Vec<u8>> = (0..n).map(|i| mp4.read_sample(&file, ti, i).expect("sample")).collect();
    println!(
        "{} {}x{} {} samples, {:.0} KB avg",
        String::from_utf8_lossy(&fourcc.0),
        v.width,
        v.height,
        n,
        samples.iter().map(Vec::len).sum::<usize>() as f64 / n as f64 / 1024.0
    );

    let run = |name: &str, make: &(dyn Fn() -> Box<dyn VideoDecoder> + Sync)| -> Vec<filmcraft_frame::VideoFrame> {
        let t0 = Instant::now();
        let chunk = n.div_ceil(threads);
        let out: Vec<Vec<filmcraft_frame::VideoFrame>> = std::thread::scope(|s| {
            let hs: Vec<_> = samples
                .chunks(chunk)
                .map(|c| {
                    s.spawn(move || {
                        let mut d = make();
                        c.iter().map(|smp| d.decode(smp, 0).expect("decode").remove(0).frame).collect::<Vec<_>>()
                    })
                })
                .collect();
            hs.into_iter().map(|h| h.join().expect("join")).collect()
        });
        let dt = t0.elapsed().as_secs_f64();
        println!("{name}: {n} frames on {threads} threads in {dt:.3} s = {:.2} ms/frame wall, {:.1} fps", dt * 1e3 / n as f64, n as f64 / dt);
        out.into_iter().flatten().collect()
    };
    let sw = || Box::new(ProResDecoder) as Box<dyn VideoDecoder>;
    let hw = || Box::new(VtProResDecoder::new(fourcc.0, v.width.into(), v.height.into()).expect("hardware decoder")) as Box<dyn VideoDecoder>;
    let a = (mode != "hw").then(|| run("software", &sw));
    let s0 = effectcraft_platform::stats();
    let b = (mode != "sw").then(|| run("hardware", &hw));
    let s1 = effectcraft_platform::stats();
    if mode != "sw" {
        println!("hardware frames {}, fallbacks {}", s1.frames - s0.frames, s1.fallbacks - s0.fallbacks);
    }
    let (Some(a), Some(b)) = (a, b) else { return };
    let mut worst = [0i32; 4];
    let mut sum = [0f64; 4];
    let mut bias = [0f64; 4];
    let mut count = [0usize; 4];
    for (fa, fb) in a.iter().zip(&b) {
        let (filmcraft_frame::PixelData::Yuv16 { planes: pa, alpha: aa, bits, .. }, filmcraft_frame::PixelData::Yuv16 { planes: pb, alpha: ab, bits: bb, .. }) =
            (&fa.data, &fb.data)
        else {
            panic!("not Yuv16")
        };
        assert_eq!(bits, bb);
        assert_eq!(fa.color, fb.color, "colour");
        assert_eq!(aa.is_some(), ab.is_some(), "alpha presence");
        let mut planes: Vec<(&Vec<u16>, &Vec<u16>)> = pa.iter().map(|p| &**p).zip(pb.iter().map(|p| &**p)).collect();
        if let (Some(x), Some(y)) = (aa, ab) {
            planes.push((x, y));
        }
        for (i, (x, y)) in planes.into_iter().enumerate() {
            assert_eq!(x.len(), y.len());
            for (p, q) in x.iter().zip(y) {
                let d = *q as i32 - *p as i32;
                worst[i] = worst[i].max(d.abs());
                sum[i] += d.abs() as f64;
                bias[i] += d as f64;
                count[i] += 1;
            }
        }
    }
    if let Ok(dir) = std::env::var("PRORES_BENCH_DUMP") {
        for (tag, f) in [("sw", &a[0]), ("hw", &b[0])] {
            if let filmcraft_frame::PixelData::Yuv16 { planes, alpha, .. } = &f.data {
                for (i, p) in planes.iter().chain(alpha.iter()).enumerate() {
                    let bytes: Vec<u8> = p.iter().flat_map(|v| v.to_le_bytes()).collect();
                    std::fs::write(format!("{dir}/{tag}{i}.raw"), bytes).expect("dump");
                }
            }
        }
    }
    // chroma differences by alpha (0, partial, opaque)
    let mut by = [[0f64; 3]; 3];
    let mut byn = [0usize; 3];
    let mut bymax = [0i32; 3];
    for (fa, fb) in a.iter().zip(&b) {
        let (filmcraft_frame::PixelData::Yuv16 { planes: pa, alpha: Some(aa), bits, .. }, filmcraft_frame::PixelData::Yuv16 { planes: pb, .. }) =
            (&fa.data, &fb.data)
        else {
            continue;
        };
        let max = (1u16 << bits) - 1;
        for i in 0..aa.len() {
            let k = if aa[i] == 0 {
                0
            } else if aa[i] == max {
                2
            } else {
                1
            };
            let d = (pb[1][i] as i32 - pa[1][i] as i32).abs();
            by[k][0] += d as f64;
            by[k][1] += (pb[0][i] as i32 - pa[0][i] as i32).abs() as f64;
            byn[k] += 1;
            bymax[k] = bymax[k].max(d);
        }
    }
    for (k, name) in ["alpha 0", "alpha partial", "alpha opaque"].iter().enumerate() {
        if byn[k] > 0 {
            println!("{name}: {} px, Cb mean |d| {:.4} max {}, Y mean |d| {:.4}", byn[k], by[k][0] / byn[k] as f64, bymax[k], by[k][1] / byn[k] as f64);
        }
    }
    for (i, name) in ["Y", "Cb", "Cr", "A"].iter().enumerate() {
        if count[i] > 0 {
            println!("{name}: max |hw-sw| {}, mean |hw-sw| {:.4}, mean hw-sw {:+.4}", worst[i], sum[i] / count[i] as f64, bias[i] / count[i] as f64);
        }
    }
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("VideoToolbox is macOS only");
}
