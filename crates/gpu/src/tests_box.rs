//! `box_h` stages its reads through workgroup memory and keeps every bit: Gaussian and box blurs
//! of random images match the one-thread-per-row `box_h_serial` exactly, at every radius, with
//! and without repeated edges, on 8 and 32 bpc pixels.
use crate::context::Enc;
use crate::effects::{box_passes, box_passes_with, box_radii, gaussian_blur};
use crate::tests;
use effectcraft_raster::Image;

/// Random premultiplied pixels: on the 8 bpc grid, or 32 bpc floats with overbrights.
fn random(seed: u32, w: u32, h: u32, eight: bool) -> Image {
    let mut img = Image::new(w, h);
    let mut s = seed.wrapping_mul(2_654_435_761).wrapping_add(1);
    let mut next = || {
        s ^= s << 13;
        s ^= s >> 17;
        s ^= s << 5;
        if eight { (s % 256) as f32 / 255.0 } else { (s >> 8) as f32 / (1u32 << 24) as f32 * 1.5 }
    };
    for y in 0..h {
        for x in 0..w {
            let a = next().min(1.0);
            img.set(x, y, [next() * a, next() * a, next() * a, a]);
        }
    }
    img
}

#[test]
fn staged_box_h_matches_the_serial_kernel_bit_for_bit() {
    let Some(g) = tests::gpu() else {
        assert!(!std::env::var("EFFECTCRAFT_REQUIRE_GPU").is_ok_and(|v| v == "1"));
        return;
    };
    let bits = |img: &Image| img.data.iter().flatten().map(|v| v.to_bits()).collect::<Vec<u32>>();
    let sizes = [(37, 5), (300, 70), (613, 131)];
    let sigmas = [0.5, 1.0, 2.7, 9.0, 40.0, 117.3, 200.0];
    for (seed, &(w, h)) in sizes.iter().enumerate() {
        for eight in [true, false] {
            let src = random(seed as u32 * 2 + eight as u32, w, h, eight);
            for repeat in [false, true] {
                let mut e = Enc::new(&g.ctx);
                let img = g.ctx.upload_image(&src).unwrap();
                for &sigma in &sigmas {
                    let new = gaussian_blur(&mut e, &img, sigma, sigma * 0.5, repeat);
                    let ry = if sigma * 0.5 > 0.05 { box_radii(sigma * 0.5, 3) } else { vec![] };
                    let old = box_passes_with(&mut e, "box_h_serial", &img, &box_radii(sigma, 3), &ry, repeat);
                    assert!(!e.take_missing());
                    let (new, old) = (e.download(&new).unwrap(), e.download(&old).unwrap());
                    assert!(new.data.iter().any(|p| p[3] > 0.0));
                    assert_eq!(bits(&new), bits(&old), "gaussian σ {sigma} {w}×{h} 8bpc {eight} repeat {repeat}");
                }
                for (r, it) in [(1, 1), (3, 2), (13, 3), (300, 1)] {
                    let new = box_passes(&mut e, &img, &vec![r; it], &[], repeat);
                    let old = box_passes_with(&mut e, "box_h_serial", &img, &vec![r; it], &[], repeat);
                    let (new, old) = (e.download(&new).unwrap(), e.download(&old).unwrap());
                    assert_eq!(bits(&new), bits(&old), "box r {r} × {it} {w}×{h} 8bpc {eight} repeat {repeat}");
                }
            }
        }
    }
}
