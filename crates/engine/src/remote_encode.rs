//! Viewer frames compressed for a network link (cloud rendering, `plan/cloud-render.md` CR4).
//!
//! A rendered frame is premultiplied RGBA8 ([`crate::remote::rgba8_premultiplied`]). On the wire
//! it travels as one of:
//! - [`Codec::Jpeg`]: the (premultiplied) colour as baseline JPEG, for scrubbing. Large frames
//!   are cut into strips of [`STRIP_ROWS`] rows, each its own JPEG, encoded and decoded on every
//!   core (one 1080p JPEG takes ~100 ms on one core; 17 strips bring it under 10). A frame that is not
//!   fully opaque also carries its alpha plane, losslessly ([`Codec::Qoi`] of a grey image), so
//!   edges and the transparency grid stay exact. Colour is clamped to alpha again after
//!   decoding, so a lossy colour never exceeds its alpha.
//! - [`Codec::Qoi`]: lossless ("Quite OK Image" format, implemented here from its public
//!   specification): the refine sent once the server is idle, or every frame when asked.
//! - [`Codec::Raw`]: the bytes themselves.
//!
//! [`fit`] scales a frame down to the size the viewer shows (an area average, premultiplied, so
//! no dark fringes). Everything here is pure Rust and portable (the browser can decode too).

use rayon::prelude::*;
use serde::{Deserialize, Serialize};

/// Rows per JPEG strip (a multiple of the 16-row MCU, so strip edges fall on block edges).
pub const STRIP_ROWS: u32 = 64;

/// How a frame's pixels are encoded on the wire.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Codec {
    #[default]
    Raw,
    Jpeg,
    Qoi,
}

/// The pixels of a frame reply: codec, encoded size, and for [`Codec::Jpeg`] the split of the
/// payload into colour (`color` bytes) and alpha (the rest, empty when opaque).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Pixels {
    pub codec: Codec,
    pub width: u32,
    pub height: u32,
    /// Bytes of the payload that are the colour JPEG (the rest is the alpha plane).
    #[serde(default)]
    pub color: u64,
    /// The colour as strips: each strip's JPEG bytes, in order (they add up to `color`), of
    /// `strip_rows` rows each (the last one fewer). Empty: one JPEG.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub strips: Vec<u64>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub strip_rows: u32,
    /// The lossless version of a frame sent lossy before (same request id).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub refine: bool,
}

/// What the client wants back: encoding and the largest size it will show.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Want {
    pub codec: Codec,
    /// JPEG quality, 1–100.
    #[serde(default = "default_quality")]
    pub quality: u8,
    /// Scale the frame down to fit `[width, height]` before encoding (never up).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_size: Option<[u32; 2]>,
    /// After a lossy frame, send the lossless version once the server is idle.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub refine: bool,
}

fn is_zero(v: &u32) -> bool {
    *v == 0
}

fn default_quality() -> u8 {
    85
}

impl Default for Want {
    /// Scrubbing: JPEG q85, refined when idle.
    fn default() -> Self {
        Want { codec: Codec::Jpeg, quality: 85, max_size: None, refine: true }
    }
}

/// [`crate::remote::rgba8_premultiplied`] on every core (a 1080p frame is 2 million pixels).
pub fn rgba8_premultiplied_par(img: &effectcraft_render::Image) -> Vec<u8> {
    let mut out = vec![0u8; img.data.len() * 4];
    out.par_chunks_mut(4 * 4096).zip(img.data.par_chunks(4096)).for_each(|(o, px)| {
        for (o, p) in o.as_chunks_mut::<4>().0.iter_mut().zip(px) {
            let a = p[3].clamp(0.0, 1.0);
            let c = |v: f32| (v.clamp(0.0, a) * 255.0 + 0.5) as u8;
            o.copy_from_slice(&[c(p[0]), c(p[1]), c(p[2]), (a * 255.0 + 0.5) as u8]);
        }
    });
    out
}

/// `rgba` (premultiplied, `w`×`h`) encoded as `codec`: the reply's [`Pixels`] and payload.
pub fn encode(rgba: &[u8], w: u32, h: u32, codec: Codec, quality: u8) -> Result<(Pixels, Vec<u8>), String> {
    let n = pixel_bytes(w, h)?;
    if rgba.len() != n {
        return Err(format!("{} bytes for a {w}×{h} frame", rgba.len()));
    }
    let px = |codec, color| Pixels { codec, width: w, height: h, color, ..Pixels::default() };
    match codec {
        Codec::Raw => Ok((px(Codec::Raw, 0), rgba.to_vec())),
        Codec::Qoi => Ok((px(Codec::Qoi, 0), qoi_encode(rgba, w, h)?)),
        Codec::Jpeg => {
            let row = w as usize * 4;
            let rows = if h > STRIP_ROWS + STRIP_ROWS / 2 { STRIP_ROWS } else { h.max(1) };
            let strips: Vec<Vec<u8>> = rgba
                .par_chunks(row * rows as usize)
                .map(|band| {
                    let rgb: Vec<u8> = band.as_chunks::<4>().0.iter().flat_map(|p| [p[0], p[1], p[2]]).collect();
                    jpeg_encode(&rgb, w, (band.len() / row.max(1)) as u32, quality)
                })
                .collect::<Result<_, _>>()?;
            let lens: Vec<u64> = strips.iter().map(|s| s.len() as u64).collect();
            let mut out = strips.concat();
            let color = out.len() as u64;
            if rgba.as_chunks::<4>().0.iter().any(|p| p[3] != 255) {
                let grey: Vec<u8> = rgba.as_chunks::<4>().0.iter().flat_map(|p| [p[3], p[3], p[3], 255]).collect();
                out.extend(qoi_encode(&grey, w, h)?);
            }
            let (strips, strip_rows) = if lens.len() > 1 { (lens, rows) } else { (vec![], 0) };
            Ok((Pixels { strips, strip_rows, ..px(Codec::Jpeg, color) }, out))
        }
    }
}

/// The premultiplied RGBA8 pixels of a payload described by `px`.
pub fn decode(px: &Pixels, payload: &[u8]) -> Result<Vec<u8>, String> {
    let n = pixel_bytes(px.width, px.height)?;
    let out = match px.codec {
        Codec::Raw => payload.to_vec(),
        Codec::Qoi => {
            let (w, h, rgba) = qoi_decode(payload)?;
            if (w, h) != (px.width, px.height) {
                return Err(format!("a {w}×{h} image for a {}×{} frame", px.width, px.height));
            }
            rgba
        }
        Codec::Jpeg => {
            let split = usize::try_from(px.color).map_err(|_| "bad colour length")?;
            let (color, alpha) = payload.split_at_checked(split).ok_or("the colour is longer than the payload")?;
            let rgb = jpeg_strips(px, color)?;
            let alpha = if alpha.is_empty() {
                None
            } else {
                let (w, h, a) = qoi_decode(alpha)?;
                if (w, h) != (px.width, px.height) {
                    return Err("the alpha plane does not match the frame".into());
                }
                Some(a)
            };
            let mut out = Vec::with_capacity(n);
            for (i, c) in rgb.as_chunks::<3>().0.iter().enumerate() {
                let a = alpha.as_ref().and_then(|a| a.get(i * 4).copied()).unwrap_or(255);
                out.extend_from_slice(&[c[0].min(a), c[1].min(a), c[2].min(a), a]);
            }
            out
        }
    };
    if out.len() != n {
        return Err(format!("{} bytes for a {}×{} frame", out.len(), px.width, px.height));
    }
    Ok(out)
}

/// The colour of a JPEG payload as RGB8, its strips decoded on every core.
fn jpeg_strips(px: &Pixels, color: &[u8]) -> Result<Vec<u8>, String> {
    let decode = |bytes: &[u8], rows: u32| -> Result<Vec<u8>, String> {
        let img = image::load_from_memory_with_format(bytes, image::ImageFormat::Jpeg).map_err(|e| format!("JPEG: {e}"))?.into_rgb8();
        if (img.width(), img.height()) != (px.width, rows) {
            return Err(format!("a {}×{} JPEG for {}×{} pixels", img.width(), img.height(), px.width, rows));
        }
        Ok(img.into_raw())
    };
    if px.strips.is_empty() {
        return decode(color, px.height);
    }
    if px.strip_rows == 0 || px.strips.len() as u64 != u64::from(px.height.div_ceil(px.strip_rows)) || px.strips.iter().sum::<u64>() != color.len() as u64 {
        return Err("the strips do not match the frame".into());
    }
    let mut parts = Vec::with_capacity(px.strips.len());
    let mut at = 0usize;
    for (i, &n) in px.strips.iter().enumerate() {
        let n = usize::try_from(n).map_err(|_| "bad strip length")?;
        let bytes = color.get(at..at.saturating_add(n)).ok_or("bad strip length")?;
        let rows = px.strip_rows.min(px.height.saturating_sub(i as u32 * px.strip_rows));
        parts.push((bytes, rows));
        at += n;
    }
    let decoded: Vec<Vec<u8>> = parts.into_par_iter().map(|(b, rows)| decode(b, rows)).collect::<Result<_, _>>()?;
    Ok(decoded.concat())
}

fn pixel_bytes(w: u32, h: u32) -> Result<usize, String> {
    usize::try_from(u64::from(w) * u64::from(h) * 4).map_err(|_| format!("a {w}×{h} frame is too large"))
}

fn jpeg_encode(rgb: &[u8], w: u32, h: u32, quality: u8) -> Result<Vec<u8>, String> {
    use image::ImageEncoder;
    let mut out = Vec::with_capacity(rgb.len() / 8);
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality.clamp(1, 100))
        .write_image(rgb, w, h, image::ExtendedColorType::Rgb8)
        .map_err(|e| format!("JPEG: {e}"))?;
    Ok(out)
}

/// The size `w`×`h` scaled down (keeping its aspect) to fit `max`; never up, never below 1.
pub fn fit_size(w: u32, h: u32, max: [u32; 2]) -> (u32, u32) {
    let [mw, mh] = [max[0].max(1), max[1].max(1)];
    if w <= mw && h <= mh {
        return (w, h);
    }
    let s = (f64::from(mw) / f64::from(w.max(1))).min(f64::from(mh) / f64::from(h.max(1)));
    let d = |v: u32| ((f64::from(v) * s).round() as u32).max(1);
    (d(w), d(h))
}

/// `rgba` (premultiplied, `w`×`h`) scaled down to fit `max` by an area average (each output
/// pixel averages the source pixels it covers). Returns the new size and pixels; the input
/// unchanged when it already fits.
pub fn fit(rgba: Vec<u8>, w: u32, h: u32, max: [u32; 2]) -> (u32, u32, Vec<u8>) {
    let (ow, oh) = fit_size(w, h, max);
    if (ow, oh) == (w, h) || rgba.len() != w as usize * h as usize * 4 {
        return (w, h, rgba);
    }
    let (w, h, ow, oh) = (w as usize, h as usize, ow as usize, oh as usize);
    // Source column span of each output column (integer bounds, at least one pixel).
    let span = |o: usize, n: usize, on: usize| (o * n / on, ((o + 1) * n / on).max(o * n / on + 1).min(n));
    let cols: Vec<(usize, usize)> = (0..ow).map(|x| span(x, w, ow)).collect();
    let mut out = vec![0u8; ow * oh * 4];
    let mut acc = vec![0u32; ow * 4];
    for (y, row_out) in out.chunks_exact_mut(ow * 4).enumerate() {
        let (y0, y1) = span(y, h, oh);
        acc.iter_mut().for_each(|a| *a = 0);
        for sy in y0..y1 {
            let Some(row) = rgba.get(sy * w * 4..(sy + 1) * w * 4) else { continue };
            for (x, &(x0, x1)) in cols.iter().enumerate() {
                for p in row.get(x0 * 4..x1 * 4).unwrap_or(&[]).as_chunks::<4>().0.iter() {
                    for c in 0..4 {
                        acc[x * 4 + c] += u32::from(p[c]);
                    }
                }
            }
        }
        for (x, &(x0, x1)) in cols.iter().enumerate() {
            let n = ((x1 - x0) * (y1 - y0)).max(1) as u32;
            for c in 0..4 {
                row_out[x * 4 + c] = ((acc[x * 4 + c] + n / 2) / n).min(255) as u8;
            }
        }
    }
    (ow as u32, oh as u32, out)
}

// QOI: https://qoiformat.org/qoi-specification.pdf (public, CC0 reference). Implemented from the
// specification: 14-byte header, ops INDEX / DIFF / LUMA / RUN / RGB / RGBA, 8-byte end marker.

const QOI_MAGIC: &[u8; 4] = b"qoif";
const QOI_END: [u8; 8] = [0, 0, 0, 0, 0, 0, 0, 1];
const OP_INDEX: u8 = 0x00;
const OP_DIFF: u8 = 0x40;
const OP_LUMA: u8 = 0x80;
const OP_RUN: u8 = 0xc0;
const OP_RGB: u8 = 0xfe;
const OP_RGBA: u8 = 0xff;
const MASK: u8 = 0xc0;

fn qoi_hash(p: [u8; 4]) -> usize {
    (usize::from(p[0]) * 3 + usize::from(p[1]) * 5 + usize::from(p[2]) * 7 + usize::from(p[3]) * 11) % 64
}

/// RGBA8 `w`×`h` as a QOI image (4 channels, sRGB flag).
pub fn qoi_encode(rgba: &[u8], w: u32, h: u32) -> Result<Vec<u8>, String> {
    if rgba.len() != pixel_bytes(w, h)? {
        return Err("QOI: the pixels do not match the size".into());
    }
    let mut out = Vec::with_capacity(rgba.len() / 3 + 22);
    out.extend_from_slice(QOI_MAGIC);
    out.extend_from_slice(&w.to_be_bytes());
    out.extend_from_slice(&h.to_be_bytes());
    out.extend_from_slice(&[4, 0]);
    let mut index = [[0u8; 4]; 64];
    let mut prev = [0u8, 0, 0, 255];
    let mut run = 0u8;
    let pixels = rgba.len() / 4;
    for (i, p) in rgba.as_chunks::<4>().0.iter().enumerate() {
        let px = [p[0], p[1], p[2], p[3]];
        if px == prev {
            run += 1;
            if run == 62 || i + 1 == pixels {
                out.push(OP_RUN | (run - 1));
                run = 0;
            }
            continue;
        }
        if run > 0 {
            out.push(OP_RUN | (run - 1));
            run = 0;
        }
        let h = qoi_hash(px);
        if index[h] == px {
            out.push(OP_INDEX | h as u8);
        } else {
            index[h] = px;
            if px[3] == prev[3] {
                let dr = px[0].wrapping_sub(prev[0]) as i8;
                let dg = px[1].wrapping_sub(prev[1]) as i8;
                let db = px[2].wrapping_sub(prev[2]) as i8;
                let (dr_dg, db_dg) = (dr.wrapping_sub(dg), db.wrapping_sub(dg));
                if (-2..=1).contains(&dr) && (-2..=1).contains(&dg) && (-2..=1).contains(&db) {
                    out.push(OP_DIFF | (((dr + 2) as u8) << 4) | (((dg + 2) as u8) << 2) | (db + 2) as u8);
                } else if (-32..=31).contains(&dg) && (-8..=7).contains(&dr_dg) && (-8..=7).contains(&db_dg) {
                    out.push(OP_LUMA | (dg + 32) as u8);
                    out.push((((dr_dg + 8) as u8) << 4) | (db_dg + 8) as u8);
                } else {
                    out.extend_from_slice(&[OP_RGB, px[0], px[1], px[2]]);
                }
            } else {
                out.extend_from_slice(&[OP_RGBA, px[0], px[1], px[2], px[3]]);
            }
        }
        prev = px;
    }
    out.extend_from_slice(&QOI_END);
    Ok(out)
}

/// A QOI image: its size and RGBA8 pixels (3-channel images come back with alpha 255).
pub fn qoi_decode(data: &[u8]) -> Result<(u32, u32, Vec<u8>), String> {
    let bad = || "QOI: truncated or damaged".to_string();
    let header = data.get(..14).ok_or_else(bad)?;
    if &header[..4] != QOI_MAGIC {
        return Err("QOI: not a QOI image".into());
    }
    let w = u32::from_be_bytes([header[4], header[5], header[6], header[7]]);
    let h = u32::from_be_bytes([header[8], header[9], header[10], header[11]]);
    let n = pixel_bytes(w, h)?;
    // A damaged header must not allocate more than the data can describe (a run op covers at
    // most 62 pixels per byte).
    if n / 4 > data.len().saturating_mul(62) {
        return Err(bad());
    }
    let mut out = Vec::with_capacity(n);
    let mut index = [[0u8; 4]; 64];
    let mut px = [0u8, 0, 0, 255];
    let mut pos = 14;
    let end = data.len().saturating_sub(QOI_END.len());
    let byte = |pos: &mut usize| -> Result<u8, String> {
        let b = *data.get(*pos).filter(|_| *pos < end).ok_or_else(bad)?;
        *pos += 1;
        Ok(b)
    };
    while out.len() < n {
        let b = byte(&mut pos)?;
        let mut run = 1usize;
        if b == OP_RGB {
            px = [byte(&mut pos)?, byte(&mut pos)?, byte(&mut pos)?, px[3]];
        } else if b == OP_RGBA {
            px = [byte(&mut pos)?, byte(&mut pos)?, byte(&mut pos)?, byte(&mut pos)?];
        } else {
            match b & MASK {
                OP_INDEX => px = index[usize::from(b & 0x3f)],
                OP_DIFF => {
                    let d = |s: u8| ((b >> s) & 3).wrapping_sub(2);
                    px = [px[0].wrapping_add(d(4)), px[1].wrapping_add(d(2)), px[2].wrapping_add(d(0)), px[3]];
                }
                OP_LUMA => {
                    let b2 = byte(&mut pos)?;
                    let dg = (b & 0x3f).wrapping_sub(32);
                    let dr = dg.wrapping_add((b2 >> 4).wrapping_sub(8));
                    let db = dg.wrapping_add((b2 & 0x0f).wrapping_sub(8));
                    px = [px[0].wrapping_add(dr), px[1].wrapping_add(dg), px[2].wrapping_add(db), px[3]];
                }
                _ => run = usize::from(b & 0x3f) + 1,
            }
        }
        index[qoi_hash(px)] = px;
        for _ in 0..run.min((n - out.len()) / 4) {
            out.extend_from_slice(&px);
        }
    }
    Ok((w, h, out))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A frame with gradients, flat areas, noise and soft alpha.
    fn frame(w: u32, h: u32) -> Vec<u8> {
        let mut seed = 0x1234_5678u32;
        let mut out = vec![];
        for y in 0..h {
            for x in 0..w {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let a = if x < w / 2 { 255 } else { (x * 255 / w) as u8 };
                let c = |v: u32| ((v % 256) as u8).min(a);
                let noise = if y > h / 2 { (seed >> 24) as u8 } else { 0 };
                out.extend_from_slice(&[c(x * 3), c(y * 2), c(u32::from(noise)), a]);
            }
        }
        out
    }

    #[test]
    fn qoi_round_trips_exactly() {
        for (w, h) in [(1, 1), (7, 5), (64, 48), (300, 2)] {
            let px = frame(w, h);
            let enc = qoi_encode(&px, w, h).unwrap();
            assert_eq!(qoi_decode(&enc).unwrap(), (w, h, px.clone()), "{w}×{h}");
            let (p, payload) = encode(&px, w, h, Codec::Qoi, 85).unwrap();
            assert_eq!(decode(&p, &payload).unwrap(), px);
        }
        // Long runs (more than 62 equal pixels) and a run at the very end.
        let flat = [9u8, 8, 7, 255].repeat(200);
        assert_eq!(qoi_decode(&qoi_encode(&flat, 20, 10).unwrap()).unwrap().2, flat);
    }

    #[test]
    fn damaged_qoi_is_an_error_not_a_crash() {
        let px = frame(32, 32);
        let enc = qoi_encode(&px, 32, 32).unwrap();
        for cut in [0, 5, 14, 20, enc.len() / 2, enc.len() - 9] {
            assert!(qoi_decode(&enc[..cut]).is_err(), "cut at {cut}");
        }
        // A header claiming a huge image with no data behind it.
        let mut huge = enc[..14].to_vec();
        huge[4..12].copy_from_slice(&[0x7f, 0xff, 0xff, 0xff, 0x7f, 0xff, 0xff, 0xff]);
        huge.extend_from_slice(&QOI_END);
        assert!(qoi_decode(&huge).is_err());
        let mut junk = enc.clone();
        for b in junk.iter_mut().skip(14).step_by(7) {
            *b ^= 0x5a;
        }
        let _ = qoi_decode(&junk);
    }

    #[test]
    fn jpeg_keeps_alpha_exact_and_colour_close() {
        let (w, h) = (64, 48);
        let px = frame(w, h);
        let (p, payload) = encode(&px, w, h, Codec::Jpeg, 90).unwrap();
        assert!(p.color > 0 && (p.color as usize) < payload.len(), "colour plus an alpha plane");
        let back = decode(&p, &payload).unwrap();
        assert_eq!(back.len(), px.len());
        for (a, b) in px.as_chunks::<4>().0.iter().zip(back.as_chunks::<4>().0.iter()) {
            assert_eq!(a[3], b[3], "alpha is lossless");
            assert!(b[0] <= b[3] && b[1] <= b[3] && b[2] <= b[3], "premultiplied");
        }
        // The smooth half is close (the noisy half is what JPEG is worst at).
        let err: u32 = px
            .as_chunks::<4>()
            .0
            .iter()
            .zip(back.as_chunks::<4>().0.iter())
            .take((w * h / 2) as usize)
            .map(|(a, b)| u32::from(a[0].abs_diff(b[0])))
            .max()
            .unwrap();
        assert!(err <= 24, "max error {err}");
        // Opaque frames carry no alpha plane.
        let opaque: Vec<u8> = px.as_chunks::<4>().0.iter().flat_map(|p| [p[0], p[1], p[2], 255]).collect();
        let (p, payload) = encode(&opaque, w, h, Codec::Jpeg, 85).unwrap();
        assert_eq!(p.color as usize, payload.len());
        assert!(decode(&p, &payload).unwrap().as_chunks::<4>().0.iter().all(|p| p[3] == 255));
        // Bad payloads.
        assert!(decode(&Pixels { color: 1 << 40, ..p.clone() }, &payload).is_err());
        assert!(decode(&Pixels { width: 3, ..p.clone() }, &payload).is_err());
        assert!(encode(&px, w + 1, h, Codec::Jpeg, 85).is_err());
    }

    #[test]
    fn parallel_conversion_matches() {
        let mut img = effectcraft_render::Image::new(77, 131);
        for (i, p) in img.data.iter_mut().enumerate() {
            let f = (i % 97) as f32 / 80.0 - 0.1;
            *p = [f, f * 0.5, 1.2 - f, (f + 0.2).min(1.0)];
        }
        assert_eq!(rgba8_premultiplied_par(&img), crate::remote::rgba8_premultiplied(&img));
    }

    #[test]
    fn tall_frames_go_as_strips() {
        let (w, h) = (40, 150);
        let px = frame(w, h);
        let (p, payload) = encode(&px, w, h, Codec::Jpeg, 90).unwrap();
        assert_eq!((p.strips.len(), p.strip_rows), (3, STRIP_ROWS));
        assert_eq!(p.strips.iter().sum::<u64>(), p.color);
        let back = decode(&p, &payload).unwrap();
        assert!(px.as_chunks::<4>().0.iter().zip(back.as_chunks::<4>().0.iter()).all(|(a, b)| a[3] == b[3]));
        let mean = px.iter().zip(&back).map(|(a, b)| f64::from(a.abs_diff(*b))).sum::<f64>() / px.len() as f64;
        assert!(mean < 6.0, "mean error {mean}");
        // Strips that don't add up are an error, not a crash.
        assert!(decode(&Pixels { strips: vec![1, 2, 3], ..p.clone() }, &payload).is_err());
        assert!(decode(&Pixels { strip_rows: 0, ..p.clone() }, &payload).is_err());
        assert!(decode(&Pixels { strips: vec![p.color, 0, 0], ..p.clone() }, &payload).is_err());
    }

    #[test]
    fn fit_averages_down_and_never_up() {
        assert_eq!(fit_size(1920, 1080, [960, 960]), (960, 540));
        assert_eq!(fit_size(100, 50, [1000, 1000]), (100, 50));
        assert_eq!(fit_size(1000, 1, [10, 10]), (10, 1));
        // 4×2 → 2×1: each output pixel is the average of a 2×2 block.
        let px: Vec<u8> = [
            [0u8, 0, 0, 0],
            [255, 255, 255, 255],
            [255, 255, 255, 255],
            [255, 255, 255, 255],
            [0, 0, 0, 0],
            [255, 255, 255, 255],
            [10, 10, 10, 255],
            [10, 10, 10, 255],
        ]
        .concat();
        let (w, h, out) = fit(px, 4, 2, [2, 2]);
        assert_eq!((w, h), (2, 1));
        assert_eq!(out, vec![128, 128, 128, 128, 133, 133, 133, 255]);
        let big = frame(37, 23);
        let (w, h, out) = fit(big, 37, 23, [10, 10]);
        assert_eq!((w, h, out.len()), (10, 6, 240));
    }
}
