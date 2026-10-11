//! Packed layer cache entries (`packed.rs`): lossless for quantised 8/16 bpc buffers, never
//! applied to anything else.

use std::sync::Arc;

use crate::packed::PackedBuf;
use crate::{Buf, LayerCache};
use effectcraft_raster::Image;

/// Deterministic premultiplied pixels (colour ≤ alpha, some transparent, some opaque).
fn noise(w: u32, h: u32, seed: u32) -> Image {
    let mut s = seed.wrapping_mul(2_654_435_761).wrapping_add(1);
    let mut next = || {
        s ^= s << 13;
        s ^= s >> 17;
        s ^= s << 5;
        (s >> 8) as f32 / (1u32 << 24) as f32
    };
    let mut img = Image::new(w, h);
    for (i, p) in img.data.iter_mut().enumerate() {
        let a = match i % 7 {
            0 => 0.0,
            1 => 1.0,
            _ => next(),
        };
        *p = [next() * a, next() * a, next() * a, a];
    }
    img
}

fn buf(img: Image) -> Buf {
    Buf { img, offset: [-3.5, 12.25], scale: 0.5 }
}

/// Insert `b` as a buffer rendered a second time (admitted to the cache proper, not on
/// probation: see `cache.rs`).
fn keep(cache: &LayerCache, key: u64, b: Arc<Buf>) {
    cache.insert(key, b.clone());
    cache.insert(key, b);
}

fn bits(b: &Buf) -> Vec<[u32; 4]> {
    b.img.data.iter().map(|p| p.map(f32::to_bits)).collect()
}

#[test]
fn quantised_buffers_round_trip_bit_exactly() {
    for (levels, px_bytes) in [(255.0, 4), (32768.0, 8)] {
        let mut img = noise(37, 23, levels as u32);
        crate::color::quantize(&mut img, levels);
        let b = buf(img);
        let p = PackedBuf::pack(&b).unwrap_or_else(|| panic!("{levels} levels packs"));
        assert_eq!(p.bytes(), b.img.data.len() * px_bytes, "{levels} levels");
        let u = p.unpack();
        assert_eq!(bits(&u), bits(&b), "{levels} levels");
        assert_eq!((u.img.width, u.img.height, u.offset, u.scale), (b.img.width, b.img.height, b.offset, b.scale));
    }
}

#[test]
fn unquantised_buffers_are_not_packed() {
    // 32 bpc: off-grid values, over-range values, negative zero.
    assert!(PackedBuf::pack(&buf(noise(16, 16, 5))).is_none());
    let mut img = noise(4, 4, 6);
    crate::color::quantize(&mut img, 255.0);
    img.data[5] = [1.5, 1.0, 1.0, 1.0];
    assert!(PackedBuf::pack(&buf(img.clone())).is_none());
    img.data[5] = [-0.0, 0.0, 0.0, 0.0];
    assert!(PackedBuf::pack(&buf(img)).is_none());
}

/// With reserved output headroom, quantised entries pack losslessly. External
/// full-buffer pins remain charged even after their indexed entries are evicted.
#[test]
fn cache_packs_under_pressure_and_counts_packed_bytes() {
    let f32_bytes = 64 * 64 * 16;
    let cache = LayerCache::new(19 * f32_bytes / 4);
    let mut kept = vec![];
    for i in 0..6u64 {
        let mut img = noise(64, 64, i as u32);
        crate::color::quantize(&mut img, 255.0);
        let b = Arc::new(buf(img));
        kept.push(bits(&b));
        keep(&cache, i, b);
    }
    let st = cache.stats();
    assert_eq!(st.entries, 6, "nothing evicted");
    assert!(st.bytes < 6 * f32_bytes, "packing saved storage");
    assert_eq!(cache.retained_stats().live_bytes, st.bytes);
    assert!(cache.retained_stats().live_bytes <= cache.budget());
    // A hot unpack is retained when both the packed source and result fit.
    cache.set_budget(8 * f32_bytes);
    let back = cache.get(0).unwrap_or_else(|| panic!("entry 0"));
    assert_eq!(bits(&back), kept[0]);
    // A hit unpacks the entry for good: the next hit shares the same buffer.
    let again = cache.get(0).unwrap_or_else(|| panic!("entry 0"));
    assert!(Arc::ptr_eq(&back, &again));
    assert_eq!(cache.stats().bytes, st.bytes + 3 * f32_bytes / 4);
    for (i, want) in kept.iter().enumerate() {
        assert_eq!(&bits(&cache.get(i as u64).unwrap_or_else(|| panic!("entry {i}"))), want, "entry {i}");
    }

    // 32 bpc: never packed, the same Arc is served back.
    let cache = LayerCache::new(4 * f32_bytes);
    let bufs: Vec<_> = (0..6u64).map(|i| Arc::new(buf(noise(64, 64, 100 + i as u32)))).collect();
    for (i, b) in bufs.iter().enumerate() {
        keep(&cache, i as u64, b.clone());
    }
    let st = cache.stats();
    assert_eq!(st.bytes, st.entries * f32_bytes);
    assert!(st.entries < 6, "evicted, not packed");
    assert!(cache.get(5).is_none(), "externally pinned older allocations deny new retention");
    assert_eq!(cache.retained_stats().live_bytes, 4 * f32_bytes);
    drop(bufs);
    let fresh = Arc::new(buf(noise(64, 64, 200)));
    keep(&cache, 6, fresh.clone());
    assert!(Arc::ptr_eq(&cache.get(6).unwrap(), &fresh));
}

/// The renderer's own buffers: an 8 bpc layer's cached content packs and unpacks exactly.
#[test]
fn rendered_8bpc_layer_packs_losslessly() {
    let mut img = noise(40, 30, 77);
    crate::color::Pipe::of(&effectcraft_project::ProjectSettings::default()).quantize(&mut img);
    let b = buf(img);
    let p = PackedBuf::pack(&b).unwrap_or_else(|| panic!("default project (8 bpc) packs"));
    assert_eq!(p.bytes(), b.img.data.len() * 4);
    assert_eq!(bits(&p.unpack()), bits(&b));
}

/// Linear blending: a cached frame is served from the blending-space copies alone (one hit per
/// layer, not two), so the working-space buffers they came from go cold; pixels are unchanged.
#[test]
fn linear_blend_hits_skip_the_working_space_copy() {
    use effectcraft_color::Label;
    use effectcraft_project::{Comp, ItemKind, LayerSource, Project, Solid, build};
    use effectcraft_time::{FrameRate, Tick};

    let mut p = Project::default();
    p.settings.blend_linear = true;
    let comp = Comp::new(40, 20, FrameRate::FPS_30, Tick::from_seconds_f64(2.0));
    let cid = p.add_item("Comp", Label::Sandstone, None, ItemKind::Comp(comp.clone().into()));
    for (i, c) in [[0.8, 0.2, 0.1], [0.1, 0.5, 0.9]].into_iter().enumerate() {
        let sid = p.add_item("S", Label::Red, None, ItemKind::Solid(Solid { color: c, width: 20 + i as u32 * 10, height: 20, pixel_aspect: 1.0 }));
        let mut l = build::layer(&mut p, &comp, "S", LayerSource::Solid { item: sid }, (20 + i as u32 * 10, 20), None);
        l.blend_mode = effectcraft_color::BlendMode::Screen;
        let Some(c) = p.comp_mut(cid) else { panic!("comp") };
        c.layers.insert(0, l);
    }
    let cache = LayerCache::default();
    let frame = || {
        let mut r = crate::Renderer::new(&p, &crate::NoFootage, crate::RenderOpts::default());
        r.cache = Some(&cache);
        r.comp_frame(cid, Tick::ZERO)
    };
    let a = frame();
    let entries = cache.stats().entries;
    assert_eq!(entries, 2, "two layers, each in the blending space alone (nothing reads the working space)");
    let hits = cache.stats().hits;
    let b = frame();
    assert_eq!(cache.stats().hits, hits + 2, "one hit per layer");
    assert_eq!(a.data, b.data);
    let uncached = crate::Renderer::new(&p, &crate::NoFootage, crate::RenderOpts::default()).comp_frame(cid, Tick::ZERO);
    assert_eq!(a.data, uncached.data);
}

#[test]
fn packed_output_reservation_covers_odd_sized_sixteen_bit_pixels() {
    let image = Image::filled(4099, 1, [1.0 / 32768.0, 0.0, 0.0, 1.0]);
    let b = buf(image);
    let p = PackedBuf::pack(&b).unwrap();
    assert_eq!(p.bytes(), 4099 * 8, "these pixels require the 16-bit attempt");
    assert_eq!(p.capacity_bytes(), Some(4099 * 8), "one requested output allocation");
    assert_eq!(bits(&p.unpack()), bits(&b));
}

#[test]
fn late_invalid_pixel_discards_partial_packed_output() {
    let mut image = Image::filled(4099, 1, [1.0 / 32768.0, 0.0, 0.0, 1.0]);
    image.data.last_mut().unwrap()[0] = -0.0;
    let b = buf(image);
    let original = bits(&b);
    assert!(PackedBuf::pack(&b).is_none());
    assert_eq!(bits(&b), original);
}
