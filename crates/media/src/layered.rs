//! Layered and vector stills: Photoshop documents (`effectcraft-psd`: the merged image, or one
//! layer for footage imported as a composition) and vector files rasterised at any scale: SVG
//! (`effectcraft-svg`), PDF / Illustrator / EPS (`effectcraft-pdf`; one layer of the file for
//! footage imported as a composition).

use std::sync::Arc;

use effectcraft_project::{AlphaMode, Footage};
use effectcraft_raster::Image;

use crate::convert::{AlphaOp, straight_to_image};
use crate::{MediaError, Result};

fn is_svg(path: &str, bytes: &[u8]) -> bool {
    path.to_ascii_lowercase().ends_with(".svg") || effectcraft_svg::looks_like_svg(bytes)
}

/// The vector document of an SVG / PDF / AI / EPS file (`None` for other formats): page
/// `page` of a PDF, restricted to the footage's layer when it names one.
pub fn vector_doc(
    path: &str,
    bytes: &[u8],
    layer: Option<&effectcraft_project::SourceLayer>,
    page: u32,
) -> Option<std::result::Result<effectcraft_svg::Doc, String>> {
    if is_svg(path, bytes) {
        return Some(effectcraft_svg::parse(bytes).map_err(|e| format!("{path}: {e}")));
    }
    effectcraft_pdf::sniff(bytes)?;
    Some(effectcraft_pdf::parse_page(bytes, page as usize).map_err(|e| format!("{path}: {e}")).and_then(|d| match layer {
        Some(l) => effectcraft_pdf::selected_layer_doc(&d, l.index as usize, &l.name, l.crop_bounds).map_err(|e| format!("{path}: {e}")),
        None => Ok(d),
    }))
}

fn is_pdf_like(path: &str, bytes: &[u8]) -> bool {
    let ext = std::path::Path::new(path).extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    effectcraft_pdf::sniff(bytes).is_some() && (effectcraft_pdf::EXTENSIONS.contains(&ext.as_str()) || bytes.starts_with(b"%PDF"))
}

/// Whether [`probe`] takes a file starting with `head` (Photoshop, SVG, PDF / Illustrator / EPS).
pub(crate) fn claims(path: &str, head: &[u8]) -> bool {
    effectcraft_psd::is_psd(head) || is_svg(path, head) || effectcraft_pdf::sniff(head).is_some()
}

/// Whether `bytes` (the first 64 KiB suffice) are a Photoshop or vector file [`probe`] handles.
/// (Unused on this fork, whose probe uses [`claims`]; kept as upstream has it.)
#[allow(dead_code)]
pub(crate) fn sniff(path: &str, bytes: &[u8]) -> bool {
    effectcraft_psd::is_psd(bytes) || is_svg(path, bytes) || is_pdf_like(path, bytes)
}

/// Parse a Photoshop document (sharing `bytes`: the parser keeps the file to read layers from).
pub(crate) fn parse_psd(path: &str, bytes: &Arc<[u8]>) -> Result<effectcraft_psd::Psd> {
    effectcraft_psd::Psd::parse(bytes.clone()).map_err(|e| MediaError::Decode(format!("{path}: {e}")))
}

/// Footage for a Photoshop or vector file (`None` for other formats).
pub(crate) fn probe(path: &str, bytes: &Arc<[u8]>) -> Result<Option<Footage>> {
    if effectcraft_psd::is_psd(bytes) {
        let psd = parse_psd(path, bytes)?;
        let mut f = crate::probe::still_footage(path, psd.width, psd.height, image::ImageFormat::Png, true);
        f.codec = if psd.psb { "PSB".into() } else { "PSD".into() };
        f.alpha = if psd.merged_alpha || !psd.layers.is_empty() { AlphaMode::Straight } else { AlphaMode::Ignore };
        return Ok(Some(f));
    }
    if is_svg(path, bytes) {
        let doc = effectcraft_svg::parse(bytes).map_err(|e| MediaError::Decode(format!("{path}: {e}")))?;
        let (w, h) = doc.pixel_size();
        let mut f = crate::probe::still_footage(path, w, h, image::ImageFormat::Png, true);
        f.codec = "SVG".into();
        f.alpha = AlphaMode::Straight;
        return Ok(Some(f));
    }
    if is_pdf_like(path, bytes) {
        let doc = effectcraft_pdf::parse(bytes).map_err(|e| MediaError::Decode(format!("{path}: {e}")))?;
        let (w, h) = doc.pixel_size();
        let mut f = crate::probe::still_footage(path, w, h, image::ImageFormat::Png, true);
        f.codec = effectcraft_pdf::codec(path, bytes).unwrap_or("PDF").into();
        f.alpha = AlphaMode::Straight;
        return Ok(Some(f));
    }
    Ok(None)
}

/// Decode a Photoshop document: its merged image, or the footage's layer or smart object.
pub(crate) fn decode_psd(path: &str, psd: &effectcraft_psd::Psd, footage: &Footage, op: AlphaOp) -> Result<Image> {
    // A smart object's embedded file: an embedded document's merged image, or an image.
    if let Some(l) = footage.layer.as_ref()
        && let Some(uuid) = l.embedded.as_deref()
    {
        let data = psd.linked_data(uuid).ok_or_else(|| MediaError::Decode(format!("{path}: no embedded file {uuid}")))?;
        let px = if effectcraft_psd::is_psd(data) {
            let inner = effectcraft_psd::Psd::parse(Arc::<[u8]>::from(data)).map_err(|e| MediaError::Decode(format!("{path} (smart object): {e}")))?;
            inner.composite().map_err(|e| MediaError::Decode(format!("{path} (smart object): {e}")))?
        } else {
            let img = crate::probe::load_oriented(data, None).map_err(|e| MediaError::Decode(format!("{path} (smart object): {e}")))?;
            let rgba = img.to_rgba32f();
            effectcraft_psd::Pixels { width: rgba.width(), height: rgba.height(), data: rgba.pixels().map(|p| p.0).collect() }
        };
        // A perspective quad or a warp: baked as placed.
        if l.placed
            && let Some(so) = psd.layers.get(l.index as usize).and_then(|pl| pl.smart_object.as_ref())
        {
            let bb = so.placed_bounds(px.width as f64, px.height as f64);
            let k = footage.width as f64 / (bb[2] - bb[0]).max(1e-9);
            if let Some((baked, _)) = so.render_placed(&px, k, Some((footage.width, footage.height))) {
                return Ok(straight_to_image(baked.width, baked.height, &baked.data, op));
            }
        }
        return Ok(straight_to_image(px.width, px.height, &px.data, op));
    }
    let px = match &footage.layer {
        Some(l) => psd.layer_pixels(l.index as usize, !l.layer_size).map(|px| match (l.layer_size, l.crop_bounds, psd.layers.get(l.index as usize)) {
            // After Effects trims a Layer Size footage to the layer's visible pixels.
            (true, Some(b), Some(rec)) if rec.fill.is_none() => crop_layer(&px, (rec.rect.left, rec.rect.top), b).unwrap_or(px),
            _ => px,
        }),
        None => psd.composite(),
    }
    .map_err(|e| MediaError::Decode(format!("{path}: {e}")))?;
    Ok(straight_to_image(px.width, px.height, &px.data, op))
}

/// Decode an SVG / PDF / Illustrator / EPS still (`None` for other formats).
pub(crate) fn decode(path: &str, bytes: &[u8], footage: &Footage, op: AlphaOp) -> Result<Option<Image>> {
    if is_svg(path, bytes) || is_pdf_like(path, bytes) {
        let doc = vector_doc(path, bytes, footage.layer.as_ref(), footage.page).unwrap_or(Err(String::new())).map_err(MediaError::Decode)?;
        let (w, h) = doc.pixel_size();
        let img = effectcraft_svg::rasterize(&doc, w, h, 1.0);
        // The rasteriser produces premultiplied pixels already.
        let _ = op;
        return Ok(Some(img));
    }
    Ok(None)
}

/// `px` (a layer's pixels with its top-left at document `origin`) cut to document `bounds`
/// [left, top, right, bottom]; transparent outside the layer. `None` for empty or oversized bounds.
fn crop_layer(px: &effectcraft_psd::Pixels, origin: (i32, i32), bounds: [f64; 4]) -> Option<effectcraft_psd::Pixels> {
    let [l, t, r, b] = bounds.map(f64::round);
    let (w, h) = (r - l, b - t);
    if !(w >= 1.0 && h >= 1.0 && w <= 30_000.0 && h <= 30_000.0 && l.is_finite() && t.is_finite()) {
        return None;
    }
    let (w, h) = (w as u32, h as u32);
    let (dx, dy) = (l as i64 - origin.0 as i64, t as i64 - origin.1 as i64);
    let mut data = vec![[0.0f32; 4]; (w as usize).checked_mul(h as usize)?];
    for (i, o) in data.iter_mut().enumerate() {
        let (x, y) = ((i % w as usize) as i64 + dx, (i / w as usize) as i64 + dy);
        if x >= 0
            && y >= 0
            && x < px.width as i64
            && y < px.height as i64
            && let Some(p) = px.data.get(y as usize * px.width as usize + x as usize)
        {
            *o = *p;
        }
    }
    Some(effectcraft_psd::Pixels { width: w, height: h, data })
}

/// A vector file rasterised at `scale` × its pixel size (Continuously Rasterize).
pub(crate) fn rasterize_vector(path: &str, bytes: &[u8], layer: Option<&effectcraft_project::SourceLayer>, page: u32, scale: f64) -> Option<Image> {
    let doc = vector_doc(path, bytes, layer, page)?.ok()?;
    let (w, h) = doc.pixel_size();
    let (sw, sh) = (((w as f64 * scale).ceil() as u32).clamp(1, 16384), ((h as f64 * scale).ceil() as u32).clamp(1, 16384));
    Some(effectcraft_svg::rasterize(&doc, sw, sh, scale))
}

#[cfg(test)]
mod crop_tests {
    use super::*;

    #[test]
    fn layer_size_footage_is_cut_to_the_visible_bounds() {
        // A 4×2 layer at document (10, 20); AE's trimmed bounds (11, 20)–(13, 23) take two
        // columns and a transparent row below the layer.
        let px = effectcraft_psd::Pixels { width: 4, height: 2, data: (0..8).map(|i| [i as f32, 0.0, 0.0, 1.0]).collect() };
        let out = crop_layer(&px, (10, 20), [11.0, 20.0, 13.0, 23.0]).unwrap();
        assert_eq!((out.width, out.height), (2, 3));
        assert_eq!(out.data[0][0], 1.0);
        assert_eq!(out.data[3][0], 6.0);
        assert_eq!(out.data[4][3], 0.0);
        assert!(crop_layer(&px, (0, 0), [5.0, 5.0, 5.0, 9.0]).is_none());
    }
}
