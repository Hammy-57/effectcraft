//! The playback stream from the command line (`plan/cloud-render.md` CR6a): `stream-bench`
//! measures rendering and H.264 chunk encoding of a comp on this machine; `stream-test` plays a
//! comp from a frame server's stream and reports each chunk's arrival against its deadline.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use effectcraft_automation::frame_server::{FrameRender, RenderJob, Rendered};
use effectcraft_engine::project::{ItemId, ItemKind, Project};
use effectcraft_engine::remote_encode;
use effectcraft_engine::render::{Backend, LayerCache, RenderOpts};
use effectcraft_engine::time::Tick;
use effectcraft_media::preview_stream::{ChunkPreset, ChunkSpec, decode_chunk, encode_chunk, even};
use serde_json::json;

use crate::{Args, Failure};

/// `--comp`: an id, or a composition's name; else the session's active comp.
pub(crate) fn find_comp(p: &Project, arg: Option<&str>, active: Option<ItemId>) -> Result<ItemId, Failure> {
    let Some(a) = arg else { return active.ok_or_else(|| Failure::Error("the project has no composition".into())) };
    if let Ok(n) = a.parse::<u64>()
        && p.comp(ItemId(n)).is_some()
    {
        return Ok(ItemId(n));
    }
    p.items
        .values()
        .find(|it| matches!(it.kind, ItemKind::Comp(_)) && it.name == a)
        .map(|it| it.id)
        .ok_or_else(|| Failure::Error(format!("no composition `{a}`")))
}

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}

/// One rendered viewer frame as premultiplied RGBA8.
pub(crate) fn rgba8(r: Rendered) -> (u32, u32, Vec<u8>) {
    match r {
        Rendered::Image(img) => (img.width, img.height, remote_encode::rgba8_premultiplied_par(&img)),
        Rendered::Rgba8 { width, height, bytes } => (width, height, bytes),
    }
}

/// `stream-bench F.ecproj [--comp C] [--start SECS] [--n FRAMES] [--gpu] [--scale S]`: render
/// `n` consecutive frames, then encode them as H.264 chunks every way the stream could.
pub fn bench(args: &Args) -> Result<(), Failure> {
    let mut s = effectcraft_host::session();
    match &args.project {
        Some(p) => s.execute("file.open", json!({"path": p})),
        None => s.execute("file.openDemoProject", json!({})),
    }
    .map_err(|e| Failure::Error(e.to_string()))?;
    let comp = find_comp(&s.project, args.opt("--comp"), s.active_comp_id())?;
    let c = s.project.comp(comp).ok_or_else(|| Failure::Error("no comp".into()))?.clone();
    let rate = c.frame_rate;
    let start = Tick::from_seconds_f64(args.num("--start").map_err(Failure::Usage)?.unwrap_or(30.0));
    let n = args.num("--n").map_err(Failure::Usage)?.unwrap_or(90.0).max(1.0) as usize;
    let scale = args.num("--scale").map_err(Failure::Usage)?.unwrap_or(1.0);
    let gpu = match args.flag("--gpu") {
        true => Some(Arc::new(effectcraft_gpu::Gpu::try_headless().map_err(|e| Failure::Error(format!("--gpu: {e}")))?)),
        false => None,
    };
    let log: effectcraft_automation::frame_server::LogFn = Arc::new(|l: &str| eprintln!("{l}"));
    let threads = crate::frame_serve::threads();
    let cache = LayerCache::new(crate::frame_serve::cache_budget());
    if let Some(g) = &gpu {
        g.context().set_cache_budget(cache.budget());
    }
    let render: Arc<dyn FrameRender> = crate::frame_serve::renderer(&s, gpu, threads, &log);
    let project = s.project.clone();
    let first = rate.frame_at(start);
    let opts = RenderOpts { scale, backend: Backend::Auto, ..Default::default() };
    println!(
        "stream-bench: comp {} ({}×{} at {}/{} fps), {n} frames from {:.2} s, scale {scale}, {threads} workers, GPU {}",
        comp.0,
        c.width,
        c.height,
        rate.num,
        rate.den,
        start.seconds(),
        render.gpu_name().unwrap_or_else(|| "off".into())
    );

    // Render: `threads` workers take frames in play order.
    let frames: Vec<Mutex<Option<(u32, u32, Vec<u8>)>>> = (0..n).map(|_| Mutex::new(None)).collect();
    let next = AtomicUsize::new(0);
    let warm = Instant::now();
    if let Ok((r, _)) = render.render(&RenderJob { project: &project, comp, time: rate.tick_of(first), opts, cache: &cache }) {
        drop(r);
    }
    println!("render: first (cold) frame {:.0} ms", ms(warm));
    let t0 = Instant::now();
    let gpu_frames = AtomicUsize::new(0);
    std::thread::scope(|sc| {
        for _ in 0..threads {
            sc.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    if i >= n {
                        break;
                    }
                    let time = rate.tick_of(first + i as i64);
                    match render.render(&RenderJob { project: &project, comp, time, opts, cache: &cache }) {
                        Ok((r, gpu)) => {
                            if gpu {
                                gpu_frames.fetch_add(1, Ordering::Relaxed);
                            }
                            if let Some(slot) = frames.get(i)
                                && let Ok(mut g) = slot.lock()
                            {
                                *g = Some(rgba8(r));
                            }
                        }
                        Err(e) => eprintln!("frame {i}: {e}"),
                    }
                }
            });
        }
    });
    let render_s = t0.elapsed().as_secs_f64();
    let frames: Vec<(u32, u32, Vec<u8>)> = frames.into_iter().filter_map(|m| m.into_inner().ok().flatten()).collect();
    let Some((w, h, _)) = frames.first().map(|f| (f.0, f.1, ())) else { return Err(Failure::Error("no frame rendered".into())) };
    println!(
        "render: {} frames {w}×{h} in {:.2} s = {:.1} fps ({} on the GPU)",
        frames.len(),
        render_s,
        frames.len() as f64 / render_s,
        gpu_frames.load(Ordering::Relaxed)
    );
    let fps = rate.as_f64();
    let bg = c.background;
    let sizes = [(w, h, "full"), (w / 2, h / 2, "half")];
    for (sw, sh, label) in sizes {
        let scaled: Vec<Vec<u8>> = if (sw, sh) == (w, h) {
            frames.iter().map(|f| f.2.clone()).collect()
        } else {
            frames.iter().map(|f| remote_encode::fit(f.2.clone(), f.0, f.1, [sw, sh]).2).collect()
        };
        let (fw, fh) = remote_encode::fit_size(w, h, [sw, sh]);
        let refs: Vec<&[u8]> = scaled.iter().map(Vec::as_slice).collect();
        let (ew, eh) = (even(fw), even(fh));
        let chunk = 15.min(refs.len());
        // One chunk on one thread, per preset and quality.
        for (preset, crf) in [(ChunkPreset::Speed, 23.0), (ChunkPreset::Speed, 28.0), (ChunkPreset::Balanced, 23.0)] {
            let spec = ChunkSpec { crf, preset, ..ChunkSpec::new(ew, eh, rate.num as u32, rate.den as u32) };
            let t = Instant::now();
            let bytes = encode_chunk(refs.get(..chunk).unwrap_or(&[]), fw, fh, bg, &spec).map_err(Failure::Error)?;
            let per = ms(t) / chunk as f64;
            let kbps = bytes.len() as f64 * 8.0 / (chunk as f64 / fps) / 1000.0;
            let t = Instant::now();
            let back = decode_chunk(&bytes, 1).map_err(Failure::Error)?;
            let dec1 = ms(t) / back.len().max(1) as f64;
            let psnr = psnr(refs.get(..chunk.min(back.len())).unwrap_or(&[]), &back.iter().map(|f| f.rgba.as_slice()).collect::<Vec<_>>(), bg);
            println!(
                "encode {label} {ew}×{eh} {preset:?} crf {crf}: 1 thread {per:.1} ms/frame ({:.1} fps), {kbps:.0} kbit/s, PSNR {psnr:.1} dB; decode 1 thread {dec1:.1} ms/frame",
                1000.0 / per
            );
        }
        // One chunk on several threads: how long the first chunk takes (warm-up).
        for t_n in [4usize, 8, 16] {
            let spec = ChunkSpec { threads: t_n, ..ChunkSpec::new(ew, eh, rate.num as u32, rate.den as u32) };
            let t = Instant::now();
            let bytes = encode_chunk(refs.get(..chunk).unwrap_or(&[]), fw, fh, bg, &spec).map_err(Failure::Error)?;
            let kbps = bytes.len() as f64 * 8.0 / (chunk as f64 / fps) / 1000.0;
            println!("encode {label} Speed crf 23: one {chunk}-frame chunk on {t_n} threads {:.0} ms ({kbps:.0} kbit/s)", ms(t));
        }
        // Many chunks at once, one thread each: throughput.
        let chunks: Vec<&[&[u8]]> = refs.chunks(chunk).collect();
        for par in [4usize, 8, 16, 32] {
            let Ok(pool) = rayon::ThreadPoolBuilder::new().num_threads(par).build() else { continue };
            let spec = ChunkSpec::new(ew, eh, rate.num as u32, rate.den as u32);
            let t = Instant::now();
            // Every chunk `par / chunks` times over, so `par` encoders are busy at once.
            let reps = par.div_ceil(chunks.len()).max(1);
            let work: Vec<&[&[u8]]> = (0..reps).flat_map(|_| chunks.iter().copied()).collect();
            let total: usize = work.iter().map(|c| c.len()).sum();
            let bytes: usize = pool.install(|| {
                use rayon::prelude::*;
                work.par_iter().map(|c| encode_chunk(c, fw, fh, bg, &spec).map(|b| b.len()).unwrap_or(0)).sum()
            });
            let secs = t.elapsed().as_secs_f64();
            println!(
                "encode {label} Speed crf 23: {} chunks, {par} at once, 1 thread each: {:.1} fps ({:.0} kbit/s)",
                work.len(),
                total as f64 / secs,
                bytes as f64 * 8.0 / (total as f64 / fps) / 1000.0
            );
        }
        // Decoding (the Mac's side, on this CPU) with several threads.
        let spec = ChunkSpec::new(ew, eh, rate.num as u32, rate.den as u32);
        if let Ok(bytes) = encode_chunk(refs.get(..chunk).unwrap_or(&[]), fw, fh, bg, &spec) {
            let t = Instant::now();
            let back = decode_chunk(&bytes, 8).map_err(Failure::Error)?;
            println!("decode {label}: 8 threads {:.1} ms/frame", ms(t) / back.len().max(1) as f64);
        }
    }
    Ok(())
}

/// Mean PSNR of decoded frames against their sources (over `bg`), in dB.
fn psnr(src: &[&[u8]], got: &[&[u8]], bg: [f32; 3]) -> f64 {
    let mut sum = 0.0;
    let mut n = 0usize;
    for (a, b) in src.iter().zip(got) {
        let mut se = 0.0f64;
        let mut k = 0usize;
        for (p, q) in a.chunks_exact(4).zip(b.chunks_exact(4)) {
            let rest = 1.0 - p[3] as f64 / 255.0;
            for c in 0..3 {
                let v = p[c] as f64 + bg[c] as f64 * 255.0 * rest;
                let d = v - q[c] as f64;
                se += d * d;
                k += 1;
            }
        }
        if k > 0 {
            let mse = se / k as f64;
            sum += if mse <= 1e-9 { 99.0 } else { 10.0 * (255.0f64 * 255.0 / mse).log10() };
            n += 1;
        }
    }
    if n == 0 { 0.0 } else { sum / n as f64 }
}
