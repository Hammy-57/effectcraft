//! Generated ProRes test pictures (no committed footage).

#![allow(dead_code)]

use filmcraft_prores::{ChromaFormat, Encoder, Frame, Profile};

/// A `w`×`h` picture with gradients, a hard edge and a checker in every plane (video range at
/// `depth` bits; full-range alpha ramp when `alpha`).
pub fn picture(w: u32, h: u32, chroma: ChromaFormat, depth: u8, alpha: bool, seed: u32) -> Frame {
    let mut f = Frame::new(w, h, chroma, depth, alpha);
    let s = u32::from(depth) - 8;
    let cw = chroma.chroma_width(w);
    let lo = 16u32 << s;
    let span = 219u32 << s;
    let cspan = 224u32 << s;
    for y in 0..h {
        for x in 0..w {
            let edge = if x > w / 2 { span / 3 } else { 0 };
            let checker = if ((x / 8) + (y / 8) + seed).is_multiple_of(2) { span / 8 } else { 0 };
            let v = lo + ((x * span / w.max(1)) / 2 + edge + checker).min(span);
            f.y[(y * w + x) as usize] = v as u16;
            if let Some(a) = f.alpha.as_mut() {
                let max = (1u32 << depth) - 1;
                a[(y * w + x) as usize] = if x < w / 8 {
                    0
                } else if x > w * 7 / 8 {
                    max as u16
                } else {
                    (x * max / w.max(1)) as u16
                };
            }
        }
        for x in 0..cw {
            let base = lo;
            let u = base + (x * cspan / cw.max(1) + y * 7 + seed * 13) % cspan;
            let v = base + ((cw - x) * cspan / cw.max(1) + if (x / 4 + y / 4).is_multiple_of(2) { cspan / 4 } else { 0 }) % cspan;
            f.cb[(y * cw + x) as usize] = u as u16;
            f.cr[(y * cw + x) as usize] = v as u16;
        }
    }
    f
}

/// One encoded ProRes frame of `picture`.
pub fn encode(profile: Profile, f: &Frame) -> Vec<u8> {
    Encoder::new(profile, f.width, f.height).encode(f).expect("encode")
}

/// Differences of two decoded pictures, scaled to 12-bit codes: (max, mean) for Y, Cb, Cr and
/// alpha (alpha `None` when neither has it).
pub fn compare(a: &filmcraft_frame::VideoFrame, b: &filmcraft_frame::VideoFrame) -> ([(u32, f64); 3], Option<(u32, f64)>) {
    use filmcraft_frame::PixelData;
    let (PixelData::Yuv16 { planes: pa, alpha: aa, bits: ba, chroma: ca }, PixelData::Yuv16 { planes: pb, alpha: ab, bits: bb, chroma: cb }) =
        (&a.data, &b.data)
    else {
        panic!("not Yuv16 pictures")
    };
    assert_eq!((ba, ca, a.width, a.height), (bb, cb, b.width, b.height), "layout");
    assert_eq!(a.color, b.color, "colour");
    assert_eq!(a.par, b.par, "pixel aspect");
    assert_eq!(aa.is_some(), ab.is_some(), "alpha presence");
    let scale = 1u32 << (12 - ba);
    let diff = |x: &[u16], y: &[u16]| {
        assert_eq!(x.len(), y.len(), "plane size");
        let (mut max, mut sum) = (0u32, 0f64);
        for (p, q) in x.iter().zip(y) {
            let d = u32::from(p.abs_diff(*q)) * scale;
            max = max.max(d);
            sum += f64::from(d);
        }
        (max, sum / x.len().max(1) as f64)
    };
    let planes = [diff(&pa[0], &pb[0]), diff(&pa[1], &pb[1]), diff(&pa[2], &pb[2])];
    let alpha = match (aa, ab) {
        (Some(x), Some(y)) => Some(diff(x, y)),
        _ => None,
    };
    (planes, alpha)
}
