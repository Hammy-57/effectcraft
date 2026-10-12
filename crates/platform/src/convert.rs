//! Decoded VideoToolbox pixel buffers (as byte rows) to the planar samples FilmCraft's ProRes
//! decoder gives: `u16` planes holding 10-bit (4:2:2) or 12-bit (4:4:4) samples, alpha at the
//! same depth. Safe code; the FFI module hands over the locked rows.

/// Planar samples of one picture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Planes {
    pub y: Vec<u16>,
    pub cb: Vec<u16>,
    pub cr: Vec<u16>,
    pub alpha: Option<Vec<u16>>,
}

/// One plane of a locked buffer: `stride` bytes a row.
#[derive(Clone, Copy)]
pub(crate) struct Rows<'a> {
    pub data: &'a [u8],
    pub stride: usize,
}

impl<'a> Rows<'a> {
    /// `len` bytes of row `y`.
    fn row(&self, y: usize, len: usize) -> Result<&'a [u8], String> {
        let start = y.checked_mul(self.stride).ok_or("row offset overflows")?;
        let end = start.checked_add(len).ok_or("row end overflows")?;
        self.data.get(start..end).ok_or_else(|| "decoded picture row out of range".to_string())
    }
}

/// A 16-bit video-range sample at `16 - shift` bits, rounded: VideoToolbox keeps more precision
/// than the coded depth, and truncating it put the picture about 0.1 code below the software
/// decoder's on average (rounded: within 0.02).
#[inline]
fn round_down(lo: u8, hi: u8, shift: u32) -> u16 {
    let v = u32::from(u16::from_le_bytes([lo, hi]));
    ((v + (1 << (shift - 1))) >> shift).min((1 << (16 - shift)) - 1) as u16
}

/// A 16-bit full-range alpha sample at `16 - shift` bits. Its low bits repeat the high ones
/// (`a << 4 | a >> 8` at 12 bits), so dropping them gives the coded value exactly; rounding would
/// add one to the upper half.
#[inline]
fn alpha_down(lo: u8, hi: u8, shift: u32) -> u16 {
    u16::from_le_bytes([lo, hi]) >> shift
}

/// Tri-planar `s4as` (`kCVPixelFormatType_444YpCbCr16VideoRange_16A_TriPlanar`: a Y' plane, an
/// interleaved Cb Cr plane at full width and an alpha plane, 16-bit little-endian, video-range
/// Y'CbCr in the high bits, full-range alpha) to 12-bit 4:4:4 planes; alpha only when `alpha`.
pub(crate) fn unpack_s4as(luma: Rows, chroma: Rows, a: Option<Rows>, w: usize, h: usize) -> Result<Planes, String> {
    let n = w.checked_mul(h).ok_or("picture size overflows")?;
    let (mut y, mut cb, mut cr) = (vec![0u16; n], vec![0u16; n], vec![0u16; n]);
    let mut alpha = a.map(|_| vec![0u16; n]);
    if w == 0 {
        return Ok(Planes { y, cb, cr, alpha });
    }
    let rows = y.chunks_exact_mut(w).zip(cb.chunks_exact_mut(w)).zip(cr.chunks_exact_mut(w));
    for (row, ((yo, cbo), cro)) in rows.enumerate() {
        for (o, b) in yo.iter_mut().zip(luma.row(row, w * 2)?.as_chunks::<2>().0) {
            *o = round_down(b[0], b[1], 4);
        }
        for ((u, v), q) in cbo.iter_mut().zip(cro.iter_mut()).zip(chroma.row(row, w * 4)?.as_chunks::<4>().0) {
            *u = round_down(q[0], q[1], 4);
            *v = round_down(q[2], q[3], 4);
        }
    }
    if let (Some(out), Some(a)) = (alpha.as_mut(), a) {
        for (row, ao) in out.chunks_exact_mut(w).enumerate() {
            for (o, b) in ao.iter_mut().zip(a.row(row, w * 2)?.as_chunks::<2>().0) {
                *o = alpha_down(b[0], b[1], 4);
            }
        }
    }
    Ok(Planes { y, cb, cr, alpha })
}

/// Biplanar `x422` (`kCVPixelFormatType_422YpCbCr10BiPlanarVideoRange`: a Y' plane and an
/// interleaved Cb Cr plane at half width, 10-bit samples in the high bits of 16-bit words) to
/// 10-bit 4:2:2 planes.
pub(crate) fn unpack_x422(luma: Rows, chroma: Rows, w: usize, h: usize) -> Result<Planes, String> {
    let cw = w.div_ceil(2);
    let n = w.checked_mul(h).ok_or("picture size overflows")?;
    let cn = cw.checked_mul(h).ok_or("picture size overflows")?;
    let (mut y, mut cb, mut cr) = (vec![0u16; n], vec![0u16; cn], vec![0u16; cn]);
    if w == 0 {
        return Ok(Planes { y, cb, cr, alpha: None });
    }
    let rows = y.chunks_exact_mut(w).zip(cb.chunks_exact_mut(cw)).zip(cr.chunks_exact_mut(cw));
    for (row, ((yo, cbo), cro)) in rows.enumerate() {
        for (o, b) in yo.iter_mut().zip(luma.row(row, w * 2)?.as_chunks::<2>().0) {
            *o = round_down(b[0], b[1], 6);
        }
        for ((u, v), q) in cbo.iter_mut().zip(cro.iter_mut()).zip(chroma.row(row, cw * 4)?.as_chunks::<4>().0) {
            *u = round_down(q[0], q[1], 6);
            *v = round_down(q[2], q[3], 6);
        }
    }
    Ok(Planes { y, cb, cr, alpha: None })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn s4as_planes_unpack_to_12_bit_planes() {
        let words = |v: &[u16]| v.iter().flat_map(|v| v.to_le_bytes()).collect::<Vec<u8>>();
        // 2×1 picture; the luma plane has 4 bytes of row padding
        let mut luma = words(&[64 << 4, (940 << 4) + 9]);
        luma.extend([0; 4]);
        let chroma = words(&[2048 << 4, 100 << 4, (1000 << 4) + 7, 0xffff]);
        let alpha = words(&[0xffff, 0x0fff]);
        fn rows(data: &[u8], stride: usize) -> Rows<'_> {
            Rows { data, stride }
        }
        let p = unpack_s4as(rows(&luma, 8), rows(&chroma, 8), Some(rows(&alpha, 4)), 2, 1).unwrap();
        assert_eq!(p.y, vec![64, 941], "rounded to the nearest 12-bit code");
        assert_eq!(p.cb, vec![2048, 1000]);
        assert_eq!(p.cr, vec![100, 4095], "clamped to 12 bits");
        assert_eq!(p.alpha, Some(vec![4095, 255]), "alpha drops its repeated low bits");
        assert_eq!(unpack_s4as(rows(&luma, 8), rows(&chroma, 8), None, 2, 1).unwrap().alpha, None);
        // planes shorter than the picture are an error, not a panic
        assert!(unpack_s4as(rows(&luma, 8), rows(&chroma, 8), None, 2, 2).is_err());
        assert!(unpack_s4as(rows(&luma, usize::MAX), rows(&chroma, 8), None, 2, 2).is_err());
    }

    #[test]
    fn x422_planes_unpack_to_10_bit_planes() {
        let words = |v: &[u16]| v.iter().flat_map(|v| (v << 6).to_le_bytes()).collect::<Vec<u8>>();
        let luma = words(&[64, 512, 940]);
        let chroma = words(&[100, 200, 300, 400]);
        let p = unpack_x422(Rows { data: &luma, stride: 6 }, Rows { data: &chroma, stride: 8 }, 3, 1).unwrap();
        assert_eq!(p.y, vec![64, 512, 940]);
        assert_eq!((p.cb, p.cr), (vec![100, 300], vec![200, 400]));
        assert!(unpack_x422(Rows { data: &luma, stride: 6 }, Rows { data: &chroma, stride: 8 }, 3, 2).is_err());
    }
}
