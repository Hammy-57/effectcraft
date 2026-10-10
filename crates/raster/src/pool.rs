//! Reusable full-canvas pixel buffers.
//!
//! Compositing a layer through a track matte, Preserve Transparency, Layer Styles or an
//! adjustment layer takes a few canvas-sized temporaries per layer per frame (a 4K `f32` canvas
//! is 132 MB). Allocating and freeing them every time churns the allocator, which keeps much of
//! what was freed. [`zeroed`] and [`copy_of`] hand out a buffer of the same size that
//! [`recycle`] gave back when there is one; the pool keeps at most [`MAX_BUFFERS`] buffers and
//! [`MAX_BYTES`] bytes (the rest is freed), so it never grows without bound.

use std::sync::{Mutex, PoisonError};

use rayon::prelude::*;

use crate::{Image, Px};

/// Buffers kept for reuse, at most.
pub const MAX_BUFFERS: usize = 8;
/// Bytes kept for reuse, at most.
pub const MAX_BYTES: usize = 256 << 20;

/// Buffers smaller than this aren't pooled (the allocator serves them well).
const MIN_PIXELS: usize = 64 * 1024;

static POOL: Mutex<Vec<Vec<Px>>> = Mutex::new(Vec::new());

fn take(len: usize) -> Option<Vec<Px>> {
    if len < MIN_PIXELS {
        return None;
    }
    let mut p = POOL.lock().unwrap_or_else(PoisonError::into_inner);
    let i = p.iter().position(|v| v.len() == len)?;
    Some(p.swap_remove(i))
}

/// A transparent `width × height` image, on a recycled buffer when one of that size is free.
pub fn zeroed(width: u32, height: u32) -> Image {
    let len = width as usize * height as usize;
    match take(len) {
        Some(mut data) => {
            data.par_iter_mut().for_each(|p| *p = [0.0; 4]);
            Image { width, height, data }
        }
        None => Image::new(width, height),
    }
}

/// A copy of `img`, on a recycled buffer when one of that size is free.
pub fn copy_of(img: &Image) -> Image {
    match take(img.data.len()) {
        Some(mut data) => {
            data.par_iter_mut().zip(img.data.par_iter()).for_each(|(d, s)| *d = *s);
            Image { width: img.width, height: img.height, data }
        }
        None => img.clone(),
    }
}

/// Give `img`'s buffer back for reuse (freed when the pool is full).
pub fn recycle(img: Image) {
    let v = img.data;
    if v.len() < MIN_PIXELS || v.len() != v.capacity() {
        return;
    }
    let bytes = |b: &[Vec<Px>]| b.iter().map(|v| v.len() * std::mem::size_of::<Px>()).sum::<usize>();
    let mut p = POOL.lock().unwrap_or_else(PoisonError::into_inner);
    let size = v.len() * std::mem::size_of::<Px>();
    if size > MAX_BYTES {
        return;
    }
    // Make room by dropping the oldest buffers (other sizes first in line).
    while !p.is_empty() && (p.len() >= MAX_BUFFERS || bytes(&p) + size > MAX_BYTES) {
        p.remove(0);
    }
    p.push(v);
}

/// Buffers and bytes in the pool (diagnostics, tests).
pub fn stats() -> (usize, usize) {
    let p = POOL.lock().unwrap_or_else(PoisonError::into_inner);
    (p.len(), p.iter().map(|v| v.len() * std::mem::size_of::<Px>()).sum())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reuses_and_bounds() {
        // (The pool is process-wide: other tests may use it, so only check what this one can.)
        let mut a = zeroed(512, 256);
        a.data[7] = [1.0; 4];
        recycle(a);
        let b = zeroed(512, 256);
        assert!(b.data.iter().all(|p| *p == [0.0; 4]), "reused buffers come back cleared");
        let c = copy_of(&Image::filled(512, 256, [0.25; 4]));
        assert!(c.data.iter().all(|p| *p == [0.25; 4]));
        recycle(b);
        recycle(c);
        // Never more than the caps, however much is recycled.
        for _ in 0..3 * MAX_BUFFERS {
            recycle(Image::new(1024, 1024));
            let (n, bytes) = stats();
            assert!(n <= MAX_BUFFERS && bytes <= MAX_BYTES, "{n} buffers, {bytes} bytes");
        }
        // Small images aren't pooled.
        recycle(Image::new(8, 8));
        assert!(stats().0 <= MAX_BUFFERS);
    }
}
