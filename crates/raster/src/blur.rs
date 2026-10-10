//! Blurs: separable box passes (exact running sums) approximating Gaussians, plus directional
//! and radial (spin/zoom) blurs.
//!
//! Box passes run straight on row-major pixels: horizontal passes per row, vertical passes over
//! blocks of rows with a per-column running sum (no transposes), ping-ponging between two
//! buffers so a Gaussian allocates two images regardless of the number of passes.

use rayon::prelude::*;

use crate::{Image, Px};

/// One horizontal box pass of radius `r` (window 2r+1) over each row of `src` (width `w`) into
/// `out`; `repeat` = edge pixels extend, otherwise transparent.
fn box_h_into<const N: usize>(src: &[[f32; N]], out: &mut [[f32; N]], w: usize, r: usize, repeat: bool) {
    if r == 0 {
        out.copy_from_slice(src);
        return;
    }
    let norm = 1.0 / (2 * r + 1) as f32;
    out.par_chunks_mut(w).zip(src.par_chunks(w)).for_each(|(o, row)| {
        let get = |i: isize| -> [f32; N] {
            if i < 0 {
                if repeat { row[0] } else { [0.0; N] }
            } else if i as usize >= w {
                if repeat { row[w - 1] } else { [0.0; N] }
            } else {
                row[i as usize]
            }
        };
        let mut acc = [0.0f32; N];
        for i in -(r as isize)..=(r as isize) {
            let p = get(i);
            for c in 0..N {
                acc[c] += p[c];
            }
        }
        // Interior pixels (both window ends inside the row) skip the edge checks; the sums are
        // the same operations in the same order.
        let (inner_lo, inner_hi) = (r.min(w), w.saturating_sub(r + 1).max(r.min(w)));
        let edge = |x: usize, acc: &mut [f32; N], o: &mut [f32; N]| {
            *o = acc.map(|v| v * norm);
            let add = get(x as isize + r as isize + 1);
            let sub = get(x as isize - r as isize);
            for c in 0..N {
                acc[c] += add[c] - sub[c];
            }
        };
        let (head, rest) = o.split_at_mut(inner_lo);
        let (mid, tail) = rest.split_at_mut(inner_hi - inner_lo);
        for (x, out) in head.iter_mut().enumerate() {
            edge(x, &mut acc, out);
        }
        for ((out, add), sub) in mid.iter_mut().zip(row.get(inner_lo + r + 1..).unwrap_or(&[])).zip(row.get(inner_lo.saturating_sub(r)..).unwrap_or(&[])) {
            *out = acc.map(|v| v * norm);
            for c in 0..N {
                acc[c] += add[c] - sub[c];
            }
        }
        for (i, out) in tail.iter_mut().enumerate() {
            edge(inner_hi + i, &mut acc, out);
        }
    });
}

/// One vertical box pass (same semantics as [`box_h_into`] along columns).
fn box_v_into<const N: usize>(src: &[[f32; N]], out: &mut [[f32; N]], w: usize, h: usize, r: usize, repeat: bool) {
    if r == 0 {
        out.copy_from_slice(src);
        return;
    }
    let norm = 1.0 / (2 * r + 1) as f32;
    let block = (4 * r).clamp(32, 256);
    let row = |i: isize| -> Option<&[[f32; N]]> {
        if i < 0 {
            repeat.then(|| &src[0..w])
        } else if i as usize >= h {
            repeat.then(|| &src[(h - 1) * w..h * w])
        } else {
            Some(&src[i as usize * w..(i as usize + 1) * w])
        }
    };
    out.par_chunks_mut(w * block).enumerate().for_each(|(bi, chunk)| {
        let ya = (bi * block) as isize;
        let mut acc = vec![[0.0f32; N]; w];
        for i in ya - r as isize..=ya + r as isize {
            if let Some(rw) = row(i) {
                for (a, p) in acc.iter_mut().zip(rw) {
                    for c in 0..N {
                        a[c] += p[c];
                    }
                }
            }
        }
        for (yy, o) in chunk.chunks_mut(w).enumerate() {
            let y = ya + yy as isize;
            for (o, a) in o.iter_mut().zip(&acc) {
                *o = a.map(|v| v * norm);
            }
            match (row(y + r as isize + 1), row(y - r as isize)) {
                (Some(add), Some(sub)) => {
                    for ((a, p), q) in acc.iter_mut().zip(add).zip(sub) {
                        for c in 0..N {
                            a[c] += p[c] - q[c];
                        }
                    }
                }
                (Some(add), None) => {
                    for (a, p) in acc.iter_mut().zip(add) {
                        for c in 0..N {
                            a[c] += p[c];
                        }
                    }
                }
                (None, Some(sub)) => {
                    for (a, q) in acc.iter_mut().zip(sub) {
                        for c in 0..N {
                            a[c] -= q[c];
                        }
                    }
                }
                (None, None) => {}
            }
        }
    });
}

/// Run horizontal passes with radii `rx` then vertical passes with radii `ry`.
fn box_passes(img: &Image, rx: &[usize], ry: &[usize], repeat: bool) -> Image {
    let (w, h) = (img.width as usize, img.height as usize);
    match box_passes_px(&img.data, w, h, rx, ry, repeat) {
        Some(data) => Image { width: img.width, height: img.height, data },
        None => img.clone(),
    }
}

/// [`box_passes`] over `w`×`h` pixels of `N` independent channels (each channel's sums are the
/// same operations in the same order whatever `N`); `None` when no pass runs.
fn box_passes_px<const N: usize>(data: &[[f32; N]], w: usize, h: usize, rx: &[usize], ry: &[usize], repeat: bool) -> Option<Vec<[f32; N]>> {
    let passes: Vec<(bool, usize)> = rx.iter().map(|&r| (false, r)).chain(ry.iter().map(|&r| (true, r))).filter(|p| p.1 > 0).collect();
    if passes.is_empty() || w == 0 || h == 0 || data.len() != w * h {
        return None;
    }
    let mut a = vec![[0.0f32; N]; w * h];
    let mut b = vec![[0.0f32; N]; w * h];
    for (i, &(vertical, r)) in passes.iter().enumerate() {
        let src: &[[f32; N]] = if i == 0 { data } else { &a };
        if vertical {
            box_v_into(src, &mut b, w, h, r, repeat);
        } else {
            box_h_into(src, &mut b, w, r, repeat);
        }
        std::mem::swap(&mut a, &mut b);
    }
    Some(a)
}

/// [`box_passes`] of one plane (a coverage mask): exactly the alpha channel of blurring it as
/// an image, at a quarter of the work.
fn box_passes_plane(data: &[f32], w: usize, h: usize, rx: &[usize], ry: &[usize], repeat: bool) -> Vec<f32> {
    let px: Vec<[f32; 1]> = data.iter().map(|&v| [v]).collect();
    match box_passes_px(&px, w, h, rx, ry, repeat) {
        Some(out) => out.into_iter().map(|[v]| v).collect(),
        None => data.to_vec(),
    }
}

/// Box radii for `n` passes approximating a Gaussian of `sigma` (Kovesi / "boxes for Gauss").
fn box_radii(sigma: f64, n: usize) -> Vec<usize> {
    if sigma <= 0.0 {
        return vec![0; n];
    }
    let w_ideal = (12.0 * sigma * sigma / n as f64 + 1.0).sqrt();
    let mut wl = w_ideal.floor() as i64;
    if wl % 2 == 0 {
        wl -= 1;
    }
    let wu = wl + 2;
    let m_ideal = (12.0 * sigma * sigma - (n as i64 * wl * wl) as f64 - 4.0 * n as f64 * wl as f64 - 3.0 * n as f64) / (-4.0 * wl as f64 - 4.0);
    let m = m_ideal.round() as i64;
    (0..n as i64).map(|i| (((if i < m { wl } else { wu }) - 1) / 2).max(0) as usize).collect()
}

/// Separable box blur with given radii, `iterations` passes.
pub fn box_blur(img: &Image, rx: usize, ry: usize, iterations: usize, repeat: bool) -> Image {
    let n = iterations.max(1);
    box_passes(img, &vec![rx; n], &vec![ry; n], repeat)
}

/// Box radii of [`gaussian_blur`] along one axis (none for a negligible sigma).
fn gaussian_radii(sigma: f64) -> Vec<usize> {
    if sigma > 0.05 { box_radii(sigma, 3) } else { vec![] }
}

/// Gaussian blur with standard deviations in pixels (3 box passes per axis).
pub fn gaussian_blur(img: &Image, sigma_x: f64, sigma_y: f64, repeat: bool) -> Image {
    box_passes(img, &gaussian_radii(sigma_x), &gaussian_radii(sigma_y), repeat)
}

/// [`box_blur`] of one `w`×`h` plane (a coverage mask): equals the alpha of [`box_blur`] of
/// the plane as an image, exactly.
pub fn box_blur_plane(data: &[f32], w: usize, h: usize, rx: usize, ry: usize, iterations: usize, repeat: bool) -> Vec<f32> {
    let n = iterations.max(1);
    box_passes_plane(data, w, h, &vec![rx; n], &vec![ry; n], repeat)
}

/// [`gaussian_blur`] of one `w`×`h` plane (a coverage mask): equals the alpha of
/// [`gaussian_blur`] of the plane as an image, exactly.
pub fn gaussian_blur_plane(data: &[f32], w: usize, h: usize, sigma_x: f64, sigma_y: f64, repeat: bool) -> Vec<f32> {
    box_passes_plane(data, w, h, &gaussian_radii(sigma_x), &gaussian_radii(sigma_y), repeat)
}

/// How far (in pixels) [`gaussian_blur`] of `sigma` spreads a pixel along its axis: the sum of
/// its box radii. Past it a transparent-edged blur of a transparent surround is zero.
pub fn gaussian_support(sigma: f64) -> usize {
    gaussian_radii(sigma).into_iter().fold(0, usize::saturating_add)
}

/// Motion blur along `angle_deg` (0 = vertical in AE's Directional Blur, measured clockwise from
/// up) over `length` pixels.
pub fn directional_blur(img: &Image, angle_deg: f64, length: f64, repeat: bool) -> Image {
    // Non-finite input has no meaningful blur (and would overflow the sample coordinates).
    if length < 0.5 || !length.is_finite() || !angle_deg.is_finite() {
        return img.clone();
    }
    // Past a billion pixels every off-centre sample is far outside any image already.
    let length = length.min(1e9);
    let n = (length.ceil() as usize).saturating_mul(2).saturating_add(1).clamp(3, 257);
    if let Some(out) = directional_blur_axis(img, angle_deg, length, n, repeat) {
        return out;
    }
    directional_blur_sampled(img, angle_deg, length, n, repeat)
}

/// One sample of [`directional_blur_sampled`]: the bilinear sample at a pixel centre plus the
/// tap's offset reads the 2×2 block whose top-left is `(kx, ky)` pixels away, with fractions
/// `(tx, ty)`, the same for every pixel.
struct DirTap {
    kx: i64,
    ky: i64,
    tx: f32,
    ty: f32,
}

/// [`directional_blur`] at any angle: `n` bilinear samples per pixel, their footprints computed
/// once per call. Pixels whose every footprint lies inside the image read the 2×2 blocks by
/// offset from the pixel's index; the rest sample per tap with the edge mode.
fn directional_blur_sampled(img: &Image, angle_deg: f64, length: f64, n: usize, repeat: bool) -> Image {
    let a = angle_deg.to_radians();
    let (dx, dy) = (a.sin(), -a.cos());
    let taps: Vec<DirTap> = (0..n)
        .map(|i| {
            let t = (i as f64 / (n - 1) as f64 - 0.5) * length;
            let (ox, oy) = (dx * t, dy * t);
            let (fx, fy) = (ox.floor(), oy.floor());
            DirTap { kx: fx as i64, ky: fy as i64, tx: (ox - fx) as f32, ty: (oy - fy) as f32 }
        })
        .collect();
    let (w, h) = (img.width as i64, img.height as i64);
    let mut out = Image::new(img.width, img.height);
    if w == 0 || h == 0 || img.data.len() as i64 != w.saturating_mul(h) {
        return out;
    }
    let (kx0, kx1) = taps.iter().fold((i64::MAX, i64::MIN), |(lo, hi), t| (lo.min(t.kx), hi.max(t.kx)));
    let (ky0, ky1) = taps.iter().fold((i64::MAX, i64::MIN), |(lo, hi), t| (lo.min(t.ky), hi.max(t.ky)));
    // Each tap's block starts this many pixels from the output pixel's index.
    let offs: Vec<i64> = taps.iter().map(|t| t.ky.saturating_mul(w).saturating_add(t.kx)).collect();
    // Columns whose blocks all fit horizontally: x + kx0 >= 0 and x + kx1 + 1 <= w - 1.
    let x_lo = kx0.saturating_neg().clamp(0, w);
    let x_hi = w.saturating_sub(1).saturating_sub(kx1).clamp(x_lo, w);
    let wu = w as usize;
    let edge = |x: i64, y: i64, t: &DirTap| -> Px {
        let (x0, y0) = (x.saturating_add(t.kx), y.saturating_add(t.ky));
        let (x1, y1) = (x0.saturating_add(1), y0.saturating_add(1));
        if repeat {
            bilerp(img.get_clamped(x0, y0), img.get_clamped(x1, y0), img.get_clamped(x0, y1), img.get_clamped(x1, y1), t.tx, t.ty)
        } else {
            bilerp(img.get(x0, y0), img.get(x1, y0), img.get(x0, y1), img.get(x1, y1), t.tx, t.ty)
        }
    };
    out.rows_mut().for_each(|(y, row)| {
        let y = y as i64;
        let rows_inside = y.saturating_add(ky0) >= 0 && y.saturating_add(ky1).saturating_add(1) < h;
        let base = y.saturating_mul(w);
        for (x, px) in row.iter_mut().enumerate() {
            let x = x as i64;
            let mut acc = [0.0f32; 4];
            if rows_inside && (x_lo..x_hi).contains(&x) {
                for (t, &off) in taps.iter().zip(&offs) {
                    // In bounds by construction; a miss (never) falls back to the edge sample.
                    let block = usize::try_from(base.saturating_add(x).saturating_add(off)).ok().and_then(|i| {
                        let (r0, r1) = (i.checked_add(wu)?, i.checked_add(wu)?.checked_add(2)?);
                        Some((img.data.get(i..i.checked_add(2)?)?, img.data.get(r0..r1)?))
                    });
                    let p = match block {
                        Some((&[a, b], &[c, d])) => bilerp(a, b, c, d, t.tx, t.ty),
                        _ => edge(x, y, t),
                    };
                    for c in 0..4 {
                        acc[c] += p[c];
                    }
                }
            } else {
                for t in &taps {
                    let p = edge(x, y, t);
                    for c in 0..4 {
                        acc[c] += p[c];
                    }
                }
            }
            *px = acc.map(|v| v / n as f32);
        }
    });
    out
}

/// [`Image::sample_bilinear`]'s interpolation of a 2×2 block.
#[inline(always)]
fn bilerp(a: Px, b: Px, c: Px, d: Px, tx: f32, ty: f32) -> Px {
    let mut o = [0.0; 4];
    for i in 0..4 {
        let top = a[i] + (b[i] - a[i]) * tx;
        let bot = c[i] + (d[i] - c[i]) * tx;
        o[i] = top + (bot - top) * ty;
    }
    o
}

/// Whole-pixel taps of an axis-aligned [`directional_blur`]: the bilinear weights its `n`
/// samples put on each pixel offset along the axis (summed, not yet divided by `n`), split into
/// a constant `run_w` over every offset in `lo..=hi` (applied as a running sum) plus the
/// residual `taps` where the weight differs from it.
struct AxisKernel {
    lo: isize,
    hi: isize,
    run_w: f64,
    taps: Vec<(isize, f64)>,
    n: f64,
}

/// Widest tap span the axis-aligned path handles; longer blurs keep the sampled path.
const AXIS_MAX_SPAN: isize = 4096;

/// The [`AxisKernel`] of `n` samples spread over `length` pixels in direction `sign` (±1).
fn axis_kernel(sign: f64, length: f64, n: usize) -> Option<AxisKernel> {
    if !length.is_finite() || n < 2 || length > AXIS_MAX_SPAN as f64 {
        return None;
    }
    let mut w: Vec<(isize, f64)> = Vec::with_capacity(2 * n);
    for i in 0..n {
        // The same offset as the sampled path, whose bilinear sample at `x + o` puts
        // `1 - frac(o)` on pixel `x + floor(o)` and `frac(o)` on the next one.
        let o = sign * ((i as f64 / (n - 1) as f64 - 0.5) * length);
        let f = o.floor();
        let k = f as isize;
        w.push((k, 1.0 - (o - f)));
        w.push((k.checked_add(1)?, o - f));
    }
    w.sort_by_key(|&(k, _)| k);
    let mut merged: Vec<(isize, f64)> = Vec::with_capacity(w.len());
    for (k, v) in w {
        match merged.last_mut() {
            Some(last) if last.0 == k => last.1 += v,
            _ => merged.push((k, v)),
        }
    }
    let (lo, hi) = (merged.first()?.0, merged.last()?.0);
    let span = hi.checked_sub(lo)?;
    if span > AXIS_MAX_SPAN {
        return None;
    }
    // Contiguous taps (sample spacing ≤ 1 px) share one weight except near the ends for whole
    // lengths: that shared weight runs as a box sum, the rest as individual taps.
    let contiguous = merged.len() as isize == span + 1;
    let run_w = if contiguous { merged.get(merged.len() / 2).map_or(0.0, |t| t.1) } else { 0.0 };
    let taps = merged.into_iter().map(|(k, v)| (k, v - run_w)).filter(|t| t.1.abs() > 1e-9).collect();
    Some(AxisKernel { lo, hi, run_w, taps, n: n as f64 })
}

/// [`directional_blur`] along an axis (0/90/180/270°, where the bilinear samples never leave
/// the pixel's row or column): a running box sum plus a few taps instead of `n` samples per
/// pixel. Equal to the sampled path to float rounding; `None` at other angles.
fn directional_blur_axis(img: &Image, angle_deg: f64, length: f64, n: usize, repeat: bool) -> Option<Image> {
    if !angle_deg.is_finite() {
        return None;
    }
    let quarter = angle_deg / 90.0;
    let q = quarter.round();
    if (quarter - q).abs() > 1e-12 {
        return None;
    }
    // Offsets along +x (90°), +y (180°), -x (270°) or -y (0°).
    let (vertical, sign) = match (q as i64).rem_euclid(4) {
        0 => (true, -1.0),
        1 => (false, 1.0),
        2 => (true, 1.0),
        _ => (false, -1.0),
    };
    let k = axis_kernel(sign, length, n)?;
    let (w, h) = (img.width as usize, img.height as usize);
    if w == 0 || h == 0 || img.data.len() != w * h {
        return None;
    }
    let mut out = Image::new(img.width, img.height);
    if vertical {
        axis_v(&img.data, &mut out.data, w, h, &k, repeat);
    } else {
        axis_h(&img.data, &mut out.data, w, &k, repeat);
    }
    Some(out)
}

/// [`AxisKernel`] along each row: the row is padded once (transparent or edge-repeated) so every
/// tap reads in bounds.
fn axis_h(src: &[Px], out: &mut [Px], w: usize, k: &AxisKernel, repeat: bool) {
    let span = k.hi.abs_diff(k.lo);
    out.par_chunks_mut(w).zip(src.par_chunks(w)).for_each(|(o, row)| {
        let (Some(&first), Some(&last)) = (row.first(), row.last()) else { return };
        let pad = |j: usize| -> Px {
            // Padded index `j` is source offset `j + lo`.
            let x = j as isize + k.lo;
            match usize::try_from(x).ok().and_then(|x| row.get(x)) {
                Some(&p) => p,
                None if !repeat => [0.0; 4],
                None if x < 0 => first,
                None => last,
            }
        };
        let padded: Vec<Px> = (0..w + span).map(pad).collect();
        let mut run = [0.0f64; 4];
        if k.run_w != 0.0 {
            for p in padded.get(..span + 1).unwrap_or(&[]) {
                for c in 0..4 {
                    run[c] += p[c] as f64;
                }
            }
        }
        for (x, px) in o.iter_mut().enumerate() {
            let mut acc = run.map(|v| v * k.run_w);
            for &(t, tw) in &k.taps {
                if let Some(p) = padded.get(x + t.abs_diff(k.lo)) {
                    for c in 0..4 {
                        acc[c] += tw * p[c] as f64;
                    }
                }
            }
            *px = acc.map(|v| (v / k.n) as f32);
            if k.run_w != 0.0 {
                let add = padded.get(x + span + 1).copied().unwrap_or([0.0; 4]);
                let sub = padded.get(x).copied().unwrap_or([0.0; 4]);
                for c in 0..4 {
                    run[c] += add[c] as f64 - sub[c] as f64;
                }
            }
        }
    });
}

/// [`AxisKernel`] along each column, whole rows at a time over blocks of output rows (a running
/// per-column sum, as in [`box_v_into`]).
fn axis_v(src: &[Px], out: &mut [Px], w: usize, h: usize, k: &AxisKernel, repeat: bool) {
    let span = k.hi.abs_diff(k.lo);
    let block = (2 * span).clamp(32, 256);
    let row = |i: isize| -> Option<&[Px]> {
        let y = if repeat { i.clamp(0, h as isize - 1) } else { i };
        let start = usize::try_from(y).ok()?.checked_mul(w)?;
        src.get(start..start.checked_add(w)?)
    };
    let add_row = |acc: &mut [[f64; 4]], rw: Option<&[Px]>, wt: f64| {
        if let Some(rw) = rw {
            for (a, p) in acc.iter_mut().zip(rw) {
                for c in 0..4 {
                    a[c] += wt * p[c] as f64;
                }
            }
        }
    };
    out.par_chunks_mut(w * block).enumerate().for_each(|(bi, chunk)| {
        let ya = (bi * block) as isize;
        let mut run = vec![[0.0f64; 4]; w];
        if k.run_w != 0.0 {
            for i in ya + k.lo..=ya + k.hi {
                add_row(&mut run, row(i), 1.0);
            }
        }
        let mut acc = vec![[0.0f64; 4]; w];
        for (yy, o) in chunk.chunks_mut(w).enumerate() {
            let y = ya + yy as isize;
            for (a, r) in acc.iter_mut().zip(&run) {
                *a = r.map(|v| v * k.run_w);
            }
            for &(t, tw) in &k.taps {
                add_row(&mut acc, row(y + t), tw);
            }
            for (o, a) in o.iter_mut().zip(&acc) {
                *o = a.map(|v| (v / k.n) as f32);
            }
            if k.run_w != 0.0 {
                add_row(&mut run, row(y + k.hi + 1), 1.0);
                add_row(&mut run, row(y + k.lo), -1.0);
            }
        }
    });
}

/// Radial blur around `center`: `spin` (degrees) or zoom (`amount` as a fraction of distance).
pub fn radial_blur(img: &Image, center: (f64, f64), amount: f64, zoom: bool) -> Image {
    if amount.abs() < 1e-6 {
        return img.clone();
    }
    let n = 32usize;
    // Per-sample scale (zoom) or rotation (spin): the same for every pixel.
    let steps: Vec<(f64, f64)> = (0..n)
        .map(|i| {
            let t = i as f64 / (n - 1) as f64 - 0.5;
            if zoom { (1.0 + t * amount, 0.0) } else { (t * amount).to_radians().sin_cos() }
        })
        .collect();
    let mut out = Image::new(img.width, img.height);
    out.rows_mut().for_each(|(y, row)| {
        for (x, px) in row.iter_mut().enumerate() {
            let (vx, vy) = (x as f64 + 0.5 - center.0, y as f64 + 0.5 - center.1);
            let mut acc = [0.0f32; 4];
            for &(s, c) in &steps {
                let (sx, sy) = if zoom { (center.0 + vx * s, center.1 + vy * s) } else { (center.0 + vx * c - vy * s, center.1 + vx * s + vy * c) };
                let p = img.sample_bilinear(sx, sy);
                for ch in 0..4 {
                    acc[ch] += p[ch];
                }
            }
            *px = acc.map(|v| v / n as f32);
        }
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Blurring one plane gives exactly that channel of blurring an image (masks, text).
    #[test]
    fn plane_blur_equals_the_channel_blur() {
        let (w, h) = (53u32, 31u32);
        let mut img = Image::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let a = if (x * 7 + y * 3) % 11 < 4 { 0.0 } else { crate::hash_noise(x, y, 1) };
                img.set(x, y, [crate::hash_noise(x, y, 2) * a, 0.3 * a, a * 0.9, a]);
            }
        }
        for c in 0..4 {
            let plane: Vec<f32> = img.data.iter().map(|p| p[c]).collect();
            for (sx, sy, repeat) in [(3.0, 1.5, false), (0.04, 7.0, true), (12.0, 0.0, false)] {
                let full = gaussian_blur(&img, sx, sy, repeat);
                let one = gaussian_blur_plane(&plane, w as usize, h as usize, sx, sy, repeat);
                assert!(full.data.iter().zip(&one).all(|(p, v)| p[c].to_bits() == v.to_bits()), "gaussian {c} {sx} {sy}");
            }
            let full = box_blur(&img, 4, 2, 1, false);
            let one = box_blur_plane(&plane, w as usize, h as usize, 4, 2, 1, false);
            assert!(full.data.iter().zip(&one).all(|(p, v)| p[c].to_bits() == v.to_bits()), "box {c}");
        }
    }

    #[test]
    fn directional_blur_repeat_keeps_an_opaque_frame_opaque() {
        // SceneYard Distorted Circle Typography: a Directional Blur adjustment layer over a
        // full-frame black solid must not fade the frame edge to the comp background.
        let mut img = Image::new(32, 16);
        img.data.iter_mut().for_each(|p| *p = [0.0, 0.0, 0.0, 1.0]);
        let fade = directional_blur(&img, 142.0, 20.0, false);
        assert!(fade.get(0, 0)[3] < 0.9);
        let kept = directional_blur(&img, 142.0, 20.0, true);
        assert!(kept.data.iter().all(|p| (p[3] - 1.0).abs() < 1e-5));
    }

    fn noisy(w: u32, h: u32) -> Image {
        let mut img = Image::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let a = if (x * 5 + y * 3) % 13 < 3 { 0.0 } else { crate::hash_noise(x, y, 7) };
                img.set(x, y, [crate::hash_noise(x, y, 8) * a, 0.6 * a, crate::hash_noise(x, y, 9) * 4.0 * a, a]);
            }
        }
        img
    }

    fn assert_close(a: &Image, b: &Image, what: &str) {
        for (i, (p, q)) in a.data.iter().zip(&b.data).enumerate() {
            for c in 0..4 {
                assert!((p[c] - q[c]).abs() <= 2e-5 * q[c].abs().max(1.0), "{what} px {i}: {p:?} vs {q:?}");
            }
        }
    }

    /// The axis-aligned Directional Blur path equals the per-pixel bilinear sampling it replaces
    /// (kept as `directional_blur_sampled`) to float rounding, at whole, fractional, sub-pixel
    /// and long lengths, both edge modes and every axis direction.
    #[test]
    fn directional_axis_path_matches_sampled() {
        let img = noisy(37, 29);
        for angle in [0.0, 90.0, 180.0, 270.0, 360.0, -90.0, 450.0] {
            for length in [0.5f64, 0.75, 1.0, 1.5, 2.0, 3.3, 7.0, 10.0, 17.25, 40.0, 127.6, 200.0, 333.0] {
                for repeat in [false, true] {
                    let n = (length.ceil() as usize * 2 + 1).clamp(3, 257);
                    let fast = directional_blur_axis(&img, angle, length, n, repeat).expect("axis angle");
                    let slow = directional_blur_ref(&img, angle, length, n, repeat);
                    assert_close(&fast, &slow, &format!("angle {angle} length {length} repeat {repeat}"));
                }
            }
        }
        assert!(directional_blur_axis(&img, 45.0, 10.0, 21, false).is_none());
        assert!(directional_blur_axis(&img, 90.5, 10.0, 21, false).is_none());
        assert!(directional_blur_axis(&img, f64::NAN, 10.0, 21, false).is_none());
    }

    /// [`directional_blur_sampled`] before its taps were precomputed: per pixel and tap, a bilinear
    /// sample at the tap's position.
    fn directional_blur_ref(img: &Image, angle_deg: f64, length: f64, n: usize, repeat: bool) -> Image {
        let a = angle_deg.to_radians();
        let (dx, dy) = (a.sin(), -a.cos());
        let mut out = Image::new(img.width, img.height);
        out.rows_mut().for_each(|(y, row)| {
            for (x, px) in row.iter_mut().enumerate() {
                let mut acc = [0.0f32; 4];
                for i in 0..n {
                    let t = (i as f64 / (n - 1) as f64 - 0.5) * length;
                    let (sx, sy) = (x as f64 + 0.5 + dx * t, y as f64 + 0.5 + dy * t);
                    let p = if repeat { img.sample_bilinear_clamped(sx, sy) } else { img.sample_bilinear(sx, sy) };
                    for c in 0..4 {
                        acc[c] += p[c];
                    }
                }
                *px = acc.map(|v| v / n as f32);
            }
        });
        out
    }

    /// The general-angle path equals the per-pixel sampling it replaces to float rounding, at
    /// angles off the axes, lengths shorter and longer than the image, both edge modes and
    /// images narrower than the blur.
    #[test]
    fn directional_sampled_matches_reference() {
        let mut max = 0.0f32;
        for img in [noisy(37, 29), noisy(1, 1), noisy(3, 2), noisy(64, 5)] {
            for angle in [45.0, 33.0, 142.0, 271.3, -17.0, 89.9, 180.0001, 1e-7] {
                for length in [0.5f64, 1.0, 3.3, 10.0, 40.0, 200.0] {
                    for repeat in [false, true] {
                        let n = (length.ceil() as usize * 2 + 1).clamp(3, 257);
                        let fast = directional_blur_sampled(&img, angle, length, n, repeat);
                        let slow = directional_blur_ref(&img, angle, length, n, repeat);
                        assert_close(&fast, &slow, &format!("angle {angle} length {length} repeat {repeat}"));
                        for (p, q) in fast.data.iter().zip(&slow.data) {
                            for c in 0..4 {
                                max = max.max((p[c] - q[c]).abs());
                            }
                        }
                    }
                }
            }
        }
        eprintln!("directional sampled vs reference: max abs difference {max:e}");
    }

    /// Degenerate inputs neither crash nor take the axis path wrongly. Regression: an infinite
    /// length overflowed the sample count and the bilinear sample coordinates.
    #[test]
    fn directional_blur_degenerate_inputs() {
        let img = noisy(5, 1);
        assert_eq!(directional_blur(&img, 0.0, f64::INFINITY, true), img);
        for (angle, length) in [(0.0, f64::INFINITY), (90.0, f64::NAN), (90.0, 1e12), (0.0, 1e5), (f64::NAN, 4.0), (33.0, 1e300)] {
            let out = directional_blur(&img, angle, length, true);
            assert_eq!(out.data.len(), img.data.len());
        }
        let empty = Image::new(0, 4);
        assert!(directional_blur(&empty, 90.0, 10.0, false).data.is_empty());
        let one = noisy(1, 1);
        let slow = directional_blur_ref(&one, 0.0, 6.0, 13, true);
        assert_close(&directional_blur(&one, 0.0, 6.0, true), &slow, "1x1");
    }

    /// Radial Blur before hoisting the per-sample angle out of the pixel loop.
    fn radial_blur_ref(img: &Image, center: (f64, f64), amount: f64, zoom: bool) -> Image {
        let n = 32usize;
        let mut out = Image::new(img.width, img.height);
        out.rows_mut().for_each(|(y, row)| {
            for (x, px) in row.iter_mut().enumerate() {
                let (vx, vy) = (x as f64 + 0.5 - center.0, y as f64 + 0.5 - center.1);
                let mut acc = [0.0f32; 4];
                for i in 0..n {
                    let t = i as f64 / (n - 1) as f64 - 0.5;
                    let (sx, sy) = if zoom {
                        let k = 1.0 + t * amount;
                        (center.0 + vx * k, center.1 + vy * k)
                    } else {
                        let (s, c) = (t * amount).to_radians().sin_cos();
                        (center.0 + vx * c - vy * s, center.1 + vx * s + vy * c)
                    };
                    let p = img.sample_bilinear(sx, sy);
                    for ch in 0..4 {
                        acc[ch] += p[ch];
                    }
                }
                *px = acc.map(|v| v / n as f32);
            }
        });
        out
    }

    #[test]
    fn radial_blur_is_bit_identical_to_reference() {
        let img = noisy(41, 23);
        for (amount, zoom) in [(30.0, false), (-7.5, false), (0.4, true), (-0.2, true)] {
            let a = radial_blur(&img, (17.3, 9.1), amount, zoom);
            let b = radial_blur_ref(&img, (17.3, 9.1), amount, zoom);
            assert!(a.data.iter().zip(&b.data).all(|(p, q)| p.map(f32::to_bits) == q.map(f32::to_bits)), "{amount} {zoom}");
        }
    }

    /// Rough timing: `cargo test -p effectcraft-raster --release --lib blur_timing -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn blur_timing() {
        let img = noisy(1920, 1080);
        for (angle, length) in [(0.0, 20.0f64), (90.0, 20.0), (90.0, 100.0), (0.0, 100.0), (45.0, 20.0), (33.0, 100.0)] {
            let n = (length.ceil() as usize * 2 + 1).clamp(3, 257);
            let t = std::time::Instant::now();
            let _ = directional_blur_ref(&img, angle, length, n, false);
            let slow = t.elapsed();
            let t = std::time::Instant::now();
            let _ = directional_blur(&img, angle, length, false);
            eprintln!("directional {angle} deg len {length}: {slow:?} -> {:?}", t.elapsed());
        }
        let t = std::time::Instant::now();
        let _ = radial_blur_ref(&img, (960.0, 540.0), 20.0, false);
        let slow = t.elapsed();
        let t = std::time::Instant::now();
        let _ = radial_blur(&img, (960.0, 540.0), 20.0, false);
        eprintln!("radial spin: {slow:?} -> {:?}", t.elapsed());
    }

    #[test]
    fn gaussian_preserves_mass() {
        let mut img = Image::new(64, 64);
        img.set(32, 32, [1.0; 4]);
        let b = gaussian_blur(&img, 4.0, 4.0, false);
        let sum: f32 = b.data.iter().map(|p| p[3]).sum();
        assert!((sum - 1.0).abs() < 1e-3, "{sum}");
        assert!(b.get(32, 32)[3] < 0.05);
    }

    #[test]
    fn flat_image_stays_flat_with_repeat() {
        let img = Image::filled(20, 10, [0.5, 0.5, 0.5, 1.0]);
        let b = gaussian_blur(&img, 3.0, 3.0, true);
        assert!(b.data.iter().all(|p| (p[0] - 0.5).abs() < 1e-5));
    }

    /// Direct (windowed sum) box filter along x or y: the definition the passes must match.
    fn box_ref(img: &Image, r: usize, vertical: bool, repeat: bool) -> Image {
        let mut out = Image::new(img.width, img.height);
        let (w, h) = (img.width as i64, img.height as i64);
        for y in 0..h {
            for x in 0..w {
                let mut acc = [0.0f64; 4];
                for k in -(r as i64)..=(r as i64) {
                    let (sx, sy) = if vertical { (x, y + k) } else { (x + k, y) };
                    let p = if repeat { img.get_clamped(sx, sy) } else { img.get(sx, sy) };
                    for c in 0..4 {
                        acc[c] += p[c] as f64;
                    }
                }
                out.set(x as u32, y as u32, acc.map(|v| (v / (2 * r + 1) as f64) as f32));
            }
        }
        out
    }

    #[test]
    fn passes_match_direct_box_filter() {
        let mut img = Image::new(53, 301);
        for y in 0..301 {
            for x in 0..53 {
                let v = crate::hash_noise(x, y, 3);
                img.set(x, y, [v * 0.5, v * 0.25, v, v]);
            }
        }
        for repeat in [false, true] {
            for (rx, ry) in [(0, 3), (2, 0), (5, 9), (40, 70), (1, 200)] {
                let fast = box_passes(&img, &[rx, rx + 1], &[ry, ry / 2], repeat);
                let mut slow = img.clone();
                for r in [rx, rx + 1] {
                    slow = box_ref(&slow, r, false, repeat);
                }
                for r in [ry, ry / 2] {
                    slow = box_ref(&slow, r, true, repeat);
                }
                for (i, (p, q)) in fast.data.iter().zip(&slow.data).enumerate() {
                    for c in 0..4 {
                        assert!((p[c] - q[c]).abs() < 1e-4, "r {rx},{ry} repeat {repeat} px {i}: {p:?} vs {q:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn radii_sane() {
        let r = box_radii(10.0, 3);
        assert_eq!(r.len(), 3);
        assert!(r.iter().all(|&v| (7..=10).contains(&v)), "{r:?}");
    }
}
