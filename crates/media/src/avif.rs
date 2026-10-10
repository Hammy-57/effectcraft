//! AVIF stills (AV1 Image File Format, a HEIF/MIAF profile), from the public AVIF and
//! ISO/IEC 23008-12 (HEIF) box layouts.
//!
//! The container is parsed here (`ftyp`, `meta`: `pitm`, `iinf`, `iloc`, `idat`, `iref`,
//! `iprp`/`ipco`/`ipma`); the AV1 payload is decoded by FilmCraft's pure-Rust AV1 decoder
//! (`filmcraft-av1`, the one MP4/WebM AV1 video uses). Supported: the
//! primary `av01` item or a `grid` of `av01` tiles, an auxiliary alpha item (straight, or
//! premultiplied when a `prem` reference says so), and the `irot` / `imir` transforms.
//! Image sequences (`avis` without a primary item) and `clap` cropping are not supported.

use std::borrow::Cow;

use effectcraft_project::{AlphaMode, Footage};
use effectcraft_raster::Image;
use filmcraft_color::{ColorInfo, Matrix, Range};
use filmcraft_frame::{Chroma, PixelData, VideoFrame};

use crate::convert::{AlphaOp, frame_to_image, straight_to_image};
use crate::{MediaError, Result};

/// Largest decoded picture we accept (pixels), so a corrupt size cannot exhaust memory.
const MAX_PIXELS: u64 = 1 << 26;
/// Auxiliary-image types that mark an alpha plane (AVIF and the older HEVC URN).
const ALPHA_URNS: &[&[u8]] = &[b"urn:mpeg:mpegB:cicp:systems:auxiliary:alpha", b"urn:mpeg:hevc:2015:auxid:1"];

type FourCc = [u8; 4];

/// Whether `bytes` is an AVIF file (an ISO BMFF `ftyp` naming the `avif` or `avis` brand).
pub(crate) fn sniff(bytes: &[u8]) -> bool {
    let Some((typ, body, _)) = next_box(bytes) else { return false };
    if &typ != b"ftyp" {
        return false;
    }
    // major brand, minor version, compatible brands
    let major = body.get(0..4);
    let compat = body.get(8..).unwrap_or(&[]);
    major.into_iter().chain(compat.chunks_exact(4)).any(|b| b == b"avif" || b == b"avis")
}

/// Footage for an AVIF still (`None` for other formats).
pub(crate) fn probe(path: &str, bytes: &[u8]) -> Result<Option<Footage>> {
    if !sniff(bytes) {
        return Ok(None);
    }
    let file = Avif::parse(bytes).map_err(|e| err(path, e))?;
    let (w, h) = match file.size(file.primary) {
        Some(s) => s,
        None => {
            let img = file.decode_item(file.primary, 0).map_err(|e| err(path, e))?;
            (img.width, img.height)
        }
    };
    let mut f = crate::probe::still_footage(path, w, h, image::ImageFormat::Png, false);
    f.codec = "AVIF".into();
    f.alpha = match file.alpha_item() {
        None => AlphaMode::Ignore,
        Some(a) if file.has_ref(b"prem", file.primary, a) => AlphaMode::Premultiplied,
        Some(_) => AlphaMode::Straight,
    };
    Ok(Some(f))
}

/// Decode an AVIF still (`None` for other formats).
pub(crate) fn decode(path: &str, bytes: &[u8], op: AlphaOp) -> Result<Option<Image>> {
    if !sniff(bytes) {
        return Ok(None);
    }
    decode_avif(bytes, op).map(Some).map_err(|e| err(path, e))
}

fn err(path: &str, e: AvifError) -> MediaError {
    match e {
        AvifError::Unsupported(s) => MediaError::Unsupported(format!("{path}: AVIF: {s}")),
        AvifError::Invalid(s) => MediaError::Decode(format!("{path}: AVIF: {s}")),
    }
}

#[derive(Debug)]
enum AvifError {
    Invalid(String),
    Unsupported(String),
}

fn invalid(s: &str) -> AvifError {
    AvifError::Invalid(s.to_string())
}

type R<T> = std::result::Result<T, AvifError>;

fn decode_avif(bytes: &[u8], op: AlphaOp) -> R<Image> {
    let file = Avif::parse(bytes)?;
    let color = file.decode_item(file.primary, 0)?;
    let alpha = match file.alpha_item() {
        Some(a) => Some(file.decode_item(a, 0)?),
        None => None,
    };
    let (w, h) = (color.width, color.height);
    let alpha = alpha.filter(|a| a.width == w && a.height == h);
    let px: Vec<[f32; 4]> = match &alpha {
        // A monochrome alpha item decodes to grey: its (range-normalised) luma is the alpha.
        Some(a) => color.data.iter().zip(&a.data).map(|(c, a)| [c[0], c[1], c[2], a[0].clamp(0.0, 1.0)]).collect(),
        None => color.data.iter().map(|c| [c[0], c[1], c[2], 1.0]).collect(),
    };
    Ok(straight_to_image(w, h, &px, op))
}

// ---------------------------------------------------------------------------------------------
// Box reading

/// The first box of `b`: (type, body, rest).
fn next_box(b: &[u8]) -> Option<(FourCc, &[u8], &[u8])> {
    let size = u32::from_be_bytes(b.get(0..4)?.try_into().ok()?) as u64;
    let typ: FourCc = b.get(4..8)?.try_into().ok()?;
    let (header, size) = match size {
        0 => (8u64, b.len() as u64),
        1 => (16, u64::from_be_bytes(b.get(8..16)?.try_into().ok()?)),
        s => (8, s),
    };
    if size < header || size > b.len() as u64 {
        return None;
    }
    let body = b.get(header as usize..size as usize)?;
    let rest = b.get(size as usize..)?;
    Some((typ, body, rest))
}

/// Child boxes of `b` (stops at the first malformed box).
fn children(mut b: &[u8]) -> Vec<(FourCc, &[u8])> {
    let mut out = Vec::new();
    while let Some((typ, body, rest)) = next_box(b) {
        out.push((typ, body));
        b = rest;
    }
    out
}

struct Rd<'a> {
    b: &'a [u8],
    pos: usize,
}

impl<'a> Rd<'a> {
    fn new(b: &'a [u8]) -> Self {
        Rd { b, pos: 0 }
    }
    fn bytes(&mut self, n: usize) -> R<&'a [u8]> {
        let end = self.pos.checked_add(n).ok_or_else(|| invalid("truncated box"))?;
        let s = self.b.get(self.pos..end).ok_or_else(|| invalid("truncated box"))?;
        self.pos = end;
        Ok(s)
    }
    fn u8(&mut self) -> R<u8> {
        Ok(self.bytes(1)?.first().copied().unwrap_or(0))
    }
    fn u16(&mut self) -> R<u16> {
        Ok(self.uint(2)? as u16)
    }
    fn u32(&mut self) -> R<u32> {
        Ok(self.uint(4)? as u32)
    }
    /// Big-endian unsigned integer of `n` (0..=8) bytes.
    fn uint(&mut self, n: usize) -> R<u64> {
        if n > 8 {
            return Err(invalid("integer field too wide"));
        }
        Ok(self.bytes(n)?.iter().fold(0u64, |v, &x| (v << 8) | x as u64))
    }
    /// FullBox header: (version, flags).
    fn full(&mut self) -> R<(u8, u32)> {
        let v = self.u32()?;
        Ok(((v >> 24) as u8, v & 0x00ff_ffff))
    }
    fn rest(&self) -> &'a [u8] {
        self.b.get(self.pos..).unwrap_or(&[])
    }
}

// ---------------------------------------------------------------------------------------------
// The item structure

struct Extent {
    /// 0: file offset, 1: offset into `idat`.
    method: u8,
    offset: u64,
    /// 0: to the end of the file / idat.
    length: u64,
}

struct Item {
    id: u32,
    typ: FourCc,
    extents: Vec<Extent>,
    /// 1-based indices into `ipco`, in association order.
    props: Vec<u16>,
}

struct Avif<'a> {
    file: &'a [u8],
    idat: &'a [u8],
    primary: u32,
    items: Vec<Item>,
    props: Vec<(FourCc, &'a [u8])>,
    /// (reference type, from, to)
    refs: Vec<(FourCc, u32, Vec<u32>)>,
}

impl<'a> Avif<'a> {
    fn parse(file: &'a [u8]) -> R<Avif<'a>> {
        let top = children(file);
        let meta = top
            .iter()
            .find(|(t, _)| t == b"meta")
            .map(|(_, b)| *b)
            .ok_or_else(|| AvifError::Unsupported("no image items (AVIF image sequences are not supported)".into()))?;
        let meta = meta.get(4..).ok_or_else(|| invalid("truncated meta box"))?;
        let boxes = children(meta);
        let find = |t: &FourCc| boxes.iter().find(|(bt, _)| bt == t).map(|(_, b)| *b);

        let mut a = Avif { file, idat: find(b"idat").unwrap_or(&[]), primary: 0, items: Vec::new(), props: Vec::new(), refs: Vec::new() };

        let pitm = find(b"pitm").ok_or_else(|| invalid("no primary item (pitm)"))?;
        let mut r = Rd::new(pitm);
        let (v, _) = r.full()?;
        a.primary = if v == 0 { r.u16()? as u32 } else { r.u32()? };

        // iinf: item types
        let iinf = find(b"iinf").ok_or_else(|| invalid("no item info (iinf)"))?;
        let mut r = Rd::new(iinf);
        let (v, _) = r.full()?;
        if v == 0 {
            r.u16()?;
        } else {
            r.u32()?;
        }
        for (t, body) in children(r.rest()) {
            if &t != b"infe" {
                continue;
            }
            let mut r = Rd::new(body);
            let (v, _) = r.full()?;
            if v < 2 {
                continue; // pre-HEIF item info without item types
            }
            let id = if v == 2 { r.u16()? as u32 } else { r.u32()? };
            r.u16()?; // protection index
            let typ: FourCc = r.bytes(4)?.try_into().map_err(|_| invalid("bad item type"))?;
            a.items.push(Item { id, typ, extents: Vec::new(), props: Vec::new() });
        }

        // iloc: where each item's data lives
        if let Some(iloc) = find(b"iloc") {
            let mut r = Rd::new(iloc);
            let (v, _) = r.full()?;
            let s = r.u8()?;
            let (offset_size, length_size) = ((s >> 4) as usize, (s & 15) as usize);
            let s = r.u8()?;
            let base_size = (s >> 4) as usize;
            let index_size = if v == 1 || v == 2 { (s & 15) as usize } else { 0 };
            let count = if v < 2 { r.u16()? as u32 } else { r.u32()? };
            for _ in 0..count {
                let id = if v < 2 { r.u16()? as u32 } else { r.u32()? };
                let method = if v == 1 || v == 2 { (r.u16()? & 15) as u8 } else { 0 };
                r.u16()?; // data reference index (0: this file)
                let base = r.uint(base_size)?;
                let n = r.u16()?;
                let mut extents = Vec::new();
                for _ in 0..n {
                    r.uint(index_size)?;
                    let off = r.uint(offset_size)?;
                    let length = r.uint(length_size)?;
                    let offset = base.checked_add(off).ok_or_else(|| invalid("extent offset overflows"))?;
                    extents.push(Extent { method, offset, length });
                }
                if let Some(it) = a.items.iter_mut().find(|i| i.id == id) {
                    it.extents = extents;
                }
            }
        }

        // iprp: properties and their associations
        if let Some(iprp) = find(b"iprp") {
            let ip = children(iprp);
            if let Some((_, ipco)) = ip.iter().find(|(t, _)| t == b"ipco") {
                a.props = children(ipco);
            }
            for (_, ipma) in ip.iter().filter(|(t, _)| t == b"ipma") {
                let mut r = Rd::new(ipma);
                let (v, flags) = r.full()?;
                let n = r.u32()?;
                for _ in 0..n {
                    let id = if v < 1 { r.u16()? as u32 } else { r.u32()? };
                    let k = r.u8()?;
                    let mut props = Vec::with_capacity(k as usize);
                    for _ in 0..k {
                        let idx = if flags & 1 != 0 { r.u16()? & 0x7fff } else { (r.u8()? & 0x7f) as u16 };
                        props.push(idx);
                    }
                    if let Some(it) = a.items.iter_mut().find(|i| i.id == id) {
                        it.props.extend(props);
                    }
                }
            }
        }

        // iref: item references (alpha, premultiplied, grid tiles)
        if let Some(iref) = find(b"iref") {
            let mut r = Rd::new(iref);
            let (v, _) = r.full()?;
            for (t, body) in children(r.rest()) {
                let mut r = Rd::new(body);
                let id = |r: &mut Rd| if v == 0 { r.u16().map(u32::from) } else { r.u32() };
                let from = id(&mut r)?;
                let n = r.u16()?;
                let to = (0..n).map(|_| id(&mut r)).collect::<R<Vec<u32>>>()?;
                a.refs.push((t, from, to));
            }
        }

        if a.item(a.primary).is_none() {
            return Err(invalid("primary item is not described"));
        }
        Ok(a)
    }

    fn item(&self, id: u32) -> Option<&Item> {
        self.items.iter().find(|i| i.id == id)
    }

    fn has_ref(&self, typ: &FourCc, from: u32, to: u32) -> bool {
        self.refs.iter().any(|(t, f, tos)| t == typ && *f == from && tos.contains(&to))
    }

    /// The item's properties of type `typ`, in association order.
    fn props_of(&self, id: u32, typ: &'a FourCc) -> impl Iterator<Item = &'a [u8]> + '_ {
        let idx = self.item(id).map(|i| i.props.as_slice()).unwrap_or(&[]);
        idx.iter().filter_map(move |&k| self.props.get((k as usize).checked_sub(1)?)).filter(move |(t, _)| t == typ).map(|(_, b)| *b)
    }

    /// The auxiliary alpha item of the primary item.
    fn alpha_item(&self) -> Option<u32> {
        self.refs.iter().filter(|(t, _, to)| t == b"auxl" && to.contains(&self.primary)).map(|(_, from, _)| *from).find(|&from| {
            self.props_of(from, b"auxC").any(|b| {
                let urn = b.get(4..).unwrap_or(&[]);
                let urn = urn.split(|&c| c == 0).next().unwrap_or(&[]);
                ALPHA_URNS.contains(&urn)
            })
        })
    }

    /// Displayed size of an item: `ispe`, swapped by a quarter-turn `irot`.
    fn size(&self, id: u32) -> Option<(u32, u32)> {
        let ispe = self.props_of(id, b"ispe").next()?;
        let mut r = Rd::new(ispe);
        r.full().ok()?;
        let (w, h) = (r.u32().ok()?, r.u32().ok()?);
        let quarter = self.props_of(id, b"irot").filter_map(|b| b.first()).fold(false, |q, a| q ^ (a & 1 == 1));
        Some(if quarter { (h, w) } else { (w, h) })
    }

    fn data(&self, item: &Item) -> R<Cow<'a, [u8]>> {
        let mut parts = Vec::with_capacity(item.extents.len());
        for e in &item.extents {
            let src = match e.method {
                0 => self.file,
                1 => self.idat,
                _ => return Err(AvifError::Unsupported("item data constructed from other items".into())),
            };
            let start = usize::try_from(e.offset).map_err(|_| invalid("extent out of range"))?;
            let end = if e.length == 0 {
                src.len()
            } else {
                usize::try_from(e.length).ok().and_then(|l| start.checked_add(l)).ok_or_else(|| invalid("extent out of range"))?
            };
            parts.push(src.get(start..end).ok_or_else(|| invalid("item data lies outside the file"))?);
        }
        Ok(match parts.as_slice() {
            [one] => Cow::Borrowed(*one),
            _ => Cow::Owned(parts.concat()),
        })
    }

    /// Decode an item to encoded straight colour (alpha 1), with its transforms applied.
    fn decode_item(&self, id: u32, depth: u32) -> R<Image> {
        let item = self.item(id).ok_or_else(|| invalid("referenced item is not described"))?;
        let img = match &item.typ {
            b"av01" => self.decode_av01(item)?,
            b"grid" if depth == 0 => self.decode_grid(item, depth)?,
            t => return Err(AvifError::Unsupported(format!("item type '{}'", String::from_utf8_lossy(t)))),
        };
        Ok(self.transform(id, img))
    }

    fn decode_av01(&self, item: &Item) -> R<Image> {
        let config = self.props_of(item.id, b"av1C").next().and_then(|b| b.get(4..)).unwrap_or(&[]);
        let data = self.data(item)?;
        let mut dec = filmcraft_av1::Decoder::new();
        let av1 = |e: filmcraft_av1::Error| AvifError::Invalid(e.to_string());
        if !config.is_empty() {
            dec.decode(config).map_err(av1)?;
        }
        let mut pics = dec.decode(&data).map_err(av1)?;
        pics.extend(dec.flush_result().map_err(av1)?);
        let pic = pics.into_iter().next().ok_or_else(|| invalid("the AV1 payload holds no picture"))?;
        if pic.width as u64 * pic.height as u64 > MAX_PIXELS {
            return Err(AvifError::Unsupported(format!("{}x{} is too large", pic.width, pic.height)));
        }
        // The `colr` nclx box overrides the bitstream's colour description; an unspecified
        // matrix means BT.601 (as AVIF readers do), not the video size-based guess.
        let nclx = self.nclx_matrix(item.id).or_else(|| self.nclx_matrix(self.primary));
        let matrix = nclx.or_else(|| Matrix::from_code(pic.matrix_coefficients)).unwrap_or(Matrix::Bt601);
        let frame = picture_to_frame(pic, matrix).ok_or_else(|| invalid("inconsistent picture planes"))?;
        Ok(frame_to_image(&frame, AlphaOp::new(AlphaMode::Ignore, [0.0; 3])))
    }

    /// The matrix an item's `colr` nclx property names (`None`: absent or unspecified).
    fn nclx_matrix(&self, id: u32) -> Option<Matrix> {
        self.props_of(id, b"colr").find_map(|b| {
            if b.get(0..4)? != b"nclx" {
                return None;
            }
            Matrix::from_code(u16::from_be_bytes(b.get(8..10)?.try_into().ok()?) as u8)
        })
    }

    /// A `grid` derived image: tiles (`dimg` references, row-major) pasted and cropped.
    fn decode_grid(&self, item: &Item, depth: u32) -> R<Image> {
        let desc = self.data(item)?;
        let mut r = Rd::new(&desc);
        r.u8()?; // version
        let flags = r.u8()?;
        let rows = r.u8()? as u32 + 1;
        let cols = r.u8()? as u32 + 1;
        let field = if flags & 1 != 0 { 4 } else { 2 };
        let (w, h) = (r.uint(field)? as u32, r.uint(field)? as u32);
        if w == 0 || h == 0 || w as u64 * h as u64 > MAX_PIXELS {
            return Err(invalid("bad grid size"));
        }
        let tiles = self.refs.iter().find(|(t, f, _)| t == b"dimg" && *f == item.id).map(|(_, _, to)| to.as_slice()).unwrap_or(&[]);
        if tiles.len() as u32 != rows * cols {
            return Err(invalid("grid tile count does not match its layout"));
        }
        let mut out = Image::new(w, h);
        let (mut tw, mut th) = (0u32, 0u32);
        for (k, &tile) in tiles.iter().enumerate() {
            let t = self.decode_item(tile, depth + 1)?;
            if k == 0 {
                (tw, th) = (t.width, t.height);
            }
            let (col, row) = (k as u32 % cols, k as u32 / cols);
            let (x0, y0) = (col.saturating_mul(tw), row.saturating_mul(th));
            for y in 0..t.height {
                let oy = y0.saturating_add(y);
                if oy >= h {
                    break;
                }
                let n = t.width.min(w.saturating_sub(x0)) as usize;
                let src = t.data.get(y as usize * t.width as usize..).and_then(|s| s.get(..n));
                let dst_start = oy as usize * w as usize + x0 as usize;
                let dst = out.data.get_mut(dst_start..dst_start.saturating_add(n));
                if let (Some(src), Some(dst)) = (src, dst) {
                    dst.copy_from_slice(src);
                }
            }
        }
        Ok(out)
    }

    /// Apply the item's `irot` (anticlockwise quarter turns) and `imir` (mirror) properties in
    /// association order.
    fn transform(&self, id: u32, mut img: Image) -> Image {
        let idx = self.item(id).map(|i| i.props.clone()).unwrap_or_default();
        for k in idx {
            let Some((t, b)) = (k as usize).checked_sub(1).and_then(|k| self.props.get(k)) else { continue };
            match t {
                b"irot" => {
                    for _ in 0..b.first().map_or(0, |a| a & 3) {
                        img = rotate_ccw(&img);
                    }
                }
                b"imir" => img = mirror(&img, b.first().is_some_and(|a| a & 1 == 1)),
                _ => {}
            }
        }
        img
    }
}

/// A decoded AV1 picture as a FilmCraft frame (monochrome gets neutral chroma).
fn picture_to_frame(p: filmcraft_av1::Picture, matrix: Matrix) -> Option<VideoFrame> {
    use std::sync::Arc;
    let color = ColorInfo { matrix, range: if p.full_range { Range::Full } else { Range::Limited }, ..ColorInfo::REC709 };
    let chroma = match (p.subsampling_x, p.subsampling_y) {
        (1, 1) => Chroma::C420,
        (1, 0) => Chroma::C422,
        _ => Chroma::C444,
    };
    let (cw, ch) = (p.plane_width(1) as usize, p.plane_height(1) as usize);
    let luma = p.width as usize * p.height as usize;
    let [y, mut u, mut v] = p.planes;
    if p.mono_chrome {
        let mid = 1u16 << p.bit_depth.clamp(1, 16).saturating_sub(1);
        u = vec![mid; cw * ch];
        v = u.clone();
    }
    if y.len() != luma || u.len() != cw * ch || v.len() != cw * ch {
        return None;
    }
    let data = if p.bit_depth == 8 {
        let to8 = |p: Vec<u16>| Arc::new(p.into_iter().map(|v| v.min(255) as u8).collect::<Vec<u8>>());
        PixelData::Yuv8 { planes: [to8(y), to8(u), to8(v)], chroma, alpha: None }
    } else {
        PixelData::Yuv16 { planes: [Arc::new(y), Arc::new(u), Arc::new(v)], chroma, bits: p.bit_depth as u32, alpha: None }
    };
    Some(VideoFrame { width: p.width, height: p.height, data, color, par: (1, 1), pts: filmcraft_time::Tick::ZERO })
}

fn rotate_ccw(img: &Image) -> Image {
    let (w, h) = (img.width as usize, img.height as usize);
    let mut out = Image::new(img.height, img.width);
    for y in 0..h {
        for x in 0..w {
            // (x, y) → (y, w-1-x) in the h-wide output
            if let (Some(s), Some(d)) = (img.data.get(y * w + x).copied(), out.data.get_mut((w - 1 - x) * h + y)) {
                *d = s;
            }
        }
    }
    out
}

/// `vertical_axis == false`: left-right mirror (axis 0 is a vertical axis); `true`: top-bottom.
fn mirror(img: &Image, top_bottom: bool) -> Image {
    let (w, h) = (img.width as usize, img.height as usize);
    let mut out = img.clone();
    if top_bottom {
        for y in 0..h {
            let (src, dst) = (img.data.get(y * w..(y + 1) * w), out.data.get_mut((h - 1 - y) * w..(h - y) * w));
            if let (Some(s), Some(d)) = (src, dst) {
                d.copy_from_slice(s);
            }
        }
    } else {
        for row in out.data.chunks_mut(w.max(1)) {
            row.reverse();
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bx(typ: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut v = ((body.len() + 8) as u32).to_be_bytes().to_vec();
        v.extend_from_slice(typ);
        v.extend_from_slice(body);
        v
    }

    #[test]
    fn sniffs_brands() {
        assert!(sniff(&bx(b"ftyp", b"avif\0\0\0\0mif1miaf")));
        assert!(sniff(&bx(b"ftyp", b"mif1\0\0\0\0avifmiaf")));
        assert!(!sniff(&bx(b"ftyp", b"isom\0\0\0\0mp41")));
        assert!(!sniff(b"\x89PNG\r\n\x1a\n"));
        assert!(!sniff(&[]));
    }

    #[test]
    fn corrupt_files_are_errors_not_crashes() {
        let ftyp = bx(b"ftyp", b"avif\0\0\0\0mif1");
        let op = AlphaOp::new(AlphaMode::Ignore, [0.0; 3]);
        // no meta
        assert!(decode("x.avif", &ftyp, op).is_err());
        // truncated / garbage meta bodies, every prefix of a plausible file
        let mut meta_body = vec![0, 0, 0, 0];
        meta_body.extend(bx(b"pitm", &[0, 0, 0, 0, 0, 1]));
        meta_body.extend(bx(b"iinf", &[0, 0, 0, 0, 0, 1]));
        meta_body.extend(bx(b"iloc", &[0, 0, 0, 0, 0x44, 0, 0, 1, 0, 1, 0, 0, 0, 1, 0, 0, 0, 0, 0xff, 0xff, 0xff, 0xff]));
        let mut file = ftyp.clone();
        file.extend(bx(b"meta", &meta_body));
        file.extend(bx(b"mdat", &[0x12, 0, 0x0a, 0x0b, 0xff, 0xff]));
        for n in 0..=file.len() {
            let _ = decode("x.avif", &file[..n], op);
            let _ = probe("x.avif", &file[..n]);
        }
        assert!(decode("x.avif", &file, op).is_err());
        // flipping bytes never panics
        for i in 0..file.len() {
            let mut f = file.clone();
            f[i] ^= 0xa5;
            let _ = decode("x.avif", &f, op);
        }
    }

    #[test]
    fn rotate_and_mirror() {
        let mut img = Image::new(2, 1);
        img.data = vec![[1.0, 0.0, 0.0, 1.0], [2.0, 0.0, 0.0, 1.0]];
        let r = rotate_ccw(&img);
        assert_eq!((r.width, r.height), (1, 2));
        // anticlockwise: the right pixel ends up on top
        assert_eq!(r.data[0][0], 2.0);
        assert_eq!(mirror(&img, false).data[0][0], 2.0);
        assert_eq!(mirror(&r, true).data[0][0], 1.0);
    }
}
