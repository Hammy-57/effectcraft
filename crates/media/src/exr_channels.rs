//! Auxiliary channels from multi-layer OpenEXR files (depth, object / material IDs, normals,
//! Cryptomatte, …) for the 3D Channel effects, read with the pure-Rust `exr` crate.
//!
//! Every channel of every layer becomes a plane named `layer.channel` (or just `channel` for
//! the unnamed layer), placed in the display window (pixels outside a layer's data window are
//! 0, or the background depth for depth channels). Cryptomatte manifests come from the
//! `cryptomatte/<key>/name` and `cryptomatte/<key>/manifest` header attributes (the published
//! Cryptomatte metadata convention: the manifest is a JSON object of name → hex hash).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use effectcraft_raster::AuxChannels;
use effectcraft_raster::channels3d::BACKGROUND_DEPTH;

fn is_depth(name: &str) -> bool {
    let last = name.rsplit('.').next().unwrap_or(name);
    last.eq_ignore_ascii_case("z") || name.to_ascii_lowercase().contains("depth")
}

/// Parse a Cryptomatte manifest (`{"name": "hexhash", …}`).
pub fn parse_manifest(json: &str) -> Vec<(String, u32)> {
    let Ok(serde_json::Value::Object(m)) = serde_json::from_str::<serde_json::Value>(json) else { return vec![] };
    let mut v: Vec<(String, u32)> = m.iter().filter_map(|(k, h)| Some((k.clone(), u32::from_str_radix(h.as_str()?.trim(), 16).ok()?))).collect();
    v.sort();
    v
}

/// Read all channels of an EXR file's bytes. `None` when it is not a readable EXR.
pub fn read_exr_channels(bytes: &[u8]) -> Option<AuxChannels> {
    use exr::prelude::*;
    let img = read().no_deep_data().largest_resolution_level().all_channels().all_layers().all_attributes().from_buffered(std::io::Cursor::new(bytes)).ok()?;
    let dw = img.attributes.display_window;
    let (w, h) = (dw.size.0, dw.size.1);
    if w == 0 || h == 0 {
        return None;
    }
    let mut aux = AuxChannels::new(w as u32, h as u32, 1.0);
    let mut crypto: HashMap<String, (Option<String>, Option<String>)> = HashMap::new();
    let mut collect_meta = |other: &HashMap<Text, AttributeValue>| {
        for (k, v) in other {
            let key = k.to_string();
            let Some(rest) = key.strip_prefix("cryptomatte/") else { continue };
            let Some((id, field)) = rest.split_once('/') else { continue };
            let AttributeValue::Text(t) = v else { continue };
            let e = crypto.entry(id.to_string()).or_default();
            match field {
                "name" => e.0 = Some(t.to_string()),
                "manifest" => e.1 = Some(t.to_string()),
                _ => {}
            }
        }
    };
    collect_meta(&img.attributes.other);
    for layer in img.layer_data.iter() {
        collect_meta(&layer.attributes.other);
        let prefix = layer.attributes.layer_name.as_ref().map(|t| t.to_string()).filter(|s| !s.is_empty());
        let (lw, lh) = (layer.size.0, layer.size.1);
        let pos = layer.attributes.layer_position;
        let (ox, oy) = (pos.0 as i64 - dw.position.0 as i64, pos.1 as i64 - dw.position.1 as i64);
        for ch in layer.channel_data.list.iter() {
            let name = match &prefix {
                Some(p) => format!("{p}.{}", ch.name),
                None => ch.name.to_string(),
            };
            let fill = if is_depth(&name) { BACKGROUND_DEPTH } else { 0.0 };
            let mut plane = vec![fill; w * h];
            let vals: Vec<f32> = ch.sample_data.values_as_f32().collect();
            if vals.len() < lw * lh {
                continue;
            }
            for y in 0..lh {
                let ty = y as i64 + oy;
                if ty < 0 || ty >= h as i64 {
                    continue;
                }
                for x in 0..lw {
                    let tx = x as i64 + ox;
                    if tx < 0 || tx >= w as i64 {
                        continue;
                    }
                    plane[ty as usize * w + tx as usize] = vals[y * lw + x];
                }
            }
            aux.channels.push((name, plane));
        }
    }
    for (_, (name, manifest)) in crypto {
        if let (Some(n), Some(m)) = (name, manifest) {
            aux.manifests.push((n, parse_manifest(&m)));
        }
    }
    aux.manifests.sort();
    Some(aux)
}

/// The picture of a multi-layer OpenEXR file without an unnamed RGB layer (Blender's
/// `ViewLayer.Combined.R`, Nuke's `beauty.R`, …), which the image decoder rejects: its colour
/// layer (preferring one named combined, beauty or rgba), else its first channel as grey.
/// Linear, straight RGBA as the file stores it. `None` when it is not a readable EXR.
pub fn layered_image(bytes: &[u8]) -> Option<image::DynamicImage> {
    let aux = read_exr_channels(bytes)?;
    let layers = aux.layers();
    // Colour layers: three different channels (not one channel, such as depth, shown in all).
    let rgb: Vec<&str> = layers
        .iter()
        .copied()
        .filter(|l| {
            let [r, g, b, _] = aux.layer_rgba(l);
            !r.is_empty() && !g.is_empty() && !b.is_empty() && r != g
        })
        .collect();
    let colour = rgb
        .iter()
        .find(|l| ["combined", "beauty", "rgba"].iter().any(|k| l.to_ascii_lowercase().contains(k)))
        .or_else(|| rgb.first())
        .or_else(|| layers.first())?;
    let names = aux.layer_rgba(colour);
    let first = aux.channels.first().map(|(n, _)| n.as_str()).unwrap_or_default();
    let plane = |k: usize| -> Option<&[f32]> { aux.get(names.get(k).map(String::as_str).filter(|n| !n.is_empty()).unwrap_or(first)) };
    let (r, g, b) = (plane(0)?, plane(1)?, plane(2)?);
    let a = names.get(3).filter(|n| !n.is_empty()).and_then(|n| aux.get(n));
    let n = (aux.width as usize).checked_mul(aux.height as usize)?;
    let mut data = Vec::with_capacity(n.checked_mul(4)?);
    for i in 0..n {
        data.extend([r.get(i)?, g.get(i)?, b.get(i)?].map(|v| *v));
        if let Some(a) = a {
            data.push(*a.get(i)?);
        }
    }
    match a {
        Some(_) => image::Rgba32FImage::from_raw(aux.width, aux.height, data).map(image::DynamicImage::ImageRgba32F),
        None => image::Rgb32FImage::from_raw(aux.width, aux.height, data).map(image::DynamicImage::ImageRgb32F),
    }
}

type Cache = Mutex<Vec<(String, Option<Arc<AuxChannels>>)>>;

/// Read (and cache, a few files) the channels of the EXR at `path` via `read`.
pub(crate) fn cached(path: &str, read: impl FnOnce() -> Option<Arc<[u8]>>) -> Option<Arc<AuxChannels>> {
    static CACHE: OnceLock<Cache> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(Vec::new()));
    if let Some(v) = cache.lock().ok().and_then(|c| c.iter().find(|(p, _)| p == path).map(|(_, v)| v.clone())) {
        return v;
    }
    let v = read().and_then(|b| read_exr_channels(&b)).map(Arc::new);
    if let Ok(mut c) = cache.lock() {
        if c.len() >= 4 {
            c.remove(0);
        }
        c.push((path.to_string(), v.clone()));
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_layered_channels_and_cryptomatte_metadata() {
        use exr::prelude::*;
        let (w, h) = (4usize, 2usize);
        let z: Vec<f32> = (0..w * h).map(|i| i as f32 * 10.0).collect();
        let id: Vec<f32> = (0..w * h).map(|i| (i % 2) as f32 + 1.0).collect();
        let r: Vec<f32> = vec![0.5; w * h];
        let channels = AnyChannels::sort(
            vec![
                AnyChannel::new("R", FlatSamples::F32(r)),
                AnyChannel::new("Z", FlatSamples::F32(z.clone())),
                AnyChannel::new("ObjectID", FlatSamples::F32(id.clone())),
            ]
            .into(),
        );
        let mut layer = Layer::new((w, h), LayerAttributes::named("depth"), Encoding::FAST_LOSSLESS, channels);
        layer.attributes.other.insert(Text::from("cryptomatte/abc1234/name"), AttributeValue::Text(Text::from("CryptoObject")));
        layer
            .attributes
            .other
            .insert(Text::from("cryptomatte/abc1234/manifest"), AttributeValue::Text(Text::from(r#"{"bunny":"13851a76","floor":"0a1b2c3d"}"#)));
        let image = Image::from_layer(layer);
        let mut bytes = std::io::Cursor::new(Vec::new());
        image.write().to_buffered(&mut bytes).unwrap();
        let aux = read_exr_channels(bytes.get_ref()).unwrap();
        assert_eq!((aux.width, aux.height), (4, 2));
        assert_eq!(aux.get("depth.Z").unwrap(), z.as_slice());
        assert_eq!(aux.depth().unwrap(), z.as_slice());
        assert_eq!(aux.object_id().unwrap(), id.as_slice());
        assert_eq!(aux.manifests, vec![("CryptoObject".to_string(), vec![("bunny".to_string(), 0x1385_1a76), ("floor".to_string(), 0x0a1b_2c3d)])]);
        assert!(read_exr_channels(b"not an exr").is_none());
    }

    /// #295: a multi-layer EXR whose colour is only in named layers (`diffuse.R`, as Blender and
    /// Nuke write them) failed to import ("does not contain non-deep rgb channels"); it now
    /// shows its colour layer and keeps every channel for EXtractoR.
    #[test]
    fn layered_exr_without_an_unnamed_rgb_layer_imports() {
        use exr::prelude::*;
        let (w, h) = (4usize, 2usize);
        let ch = |n: &str, v: f32| AnyChannel::new(n, FlatSamples::F32(vec![v; w * h]));
        let channels = AnyChannels::sort(vec![ch("diffuse.R", 1.0), ch("diffuse.G", 0.2), ch("diffuse.B", 0.0), ch("depth.Z", 7.0)].into());
        let image = Image::from_layer(Layer::new((w, h), LayerAttributes::default(), Encoding::FAST_LOSSLESS, channels));
        let mut bytes = std::io::Cursor::new(Vec::new());
        image.write().to_buffered(&mut bytes).unwrap();
        let b: Arc<[u8]> = bytes.into_inner().into();
        let f = crate::probe_bytes("/layers.exr", b.clone()).unwrap();
        assert_eq!((f.width, f.height, f.alpha), (4, 2, effectcraft_project::AlphaMode::Ignore));
        let pool = crate::MediaPool::new();
        pool.add_bytes("/layers.exr", b.clone());
        let px = pool.frame_at(&f, effectcraft_time::Tick::ZERO).unwrap().get(1, 1);
        assert!((px[0] - 1.0).abs() < 1e-4 && px[1] > 0.3 && px[1] < 0.6 && px[2] < 1e-4 && px[3] == 1.0, "diffuse, sRGB-encoded: {px:?}");
        let aux = read_exr_channels(&b).unwrap();
        assert_eq!(aux.layers(), ["depth", "diffuse"]);
        assert_eq!(aux.layer_rgba("diffuse"), ["diffuse.R", "diffuse.G", "diffuse.B", ""].map(String::from));
    }
}
