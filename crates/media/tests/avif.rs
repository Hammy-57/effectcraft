//! AVIF stills: files built here from `effectcraft-av1enc` output, decoded through the probe
//! and the media pool (FilmCraft's AV1 decoder underneath).

use std::sync::Arc;

use effectcraft_av1enc::{Encoder, EncoderConfig, Frame, RateControl};
use effectcraft_media::{MediaPool, probe_bytes};
use effectcraft_project::{AlphaMode, FootageKind};
use effectcraft_time::Tick;

/// A 4:2:0 8-bit picture: luma `y(x)`, flat chroma `(u, v)`. Returns (av1C, sample).
fn encode(w: usize, h: usize, y: impl Fn(usize) -> u16, u: u16, v: u16) -> (Vec<u8>, Vec<u8>) {
    let mut c = EncoderConfig::new(w as u32, h as u32, 30, 1);
    c.rate = RateControl::ConstantQ(20);
    c.keyint = 1;
    let mut enc = Encoder::new(c).expect("encoder");
    let ys: Vec<u16> = (0..w * h).map(|i| y(i % w)).collect();
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let us = vec![u; cw * ch];
    let vs = vec![v; cw * ch];
    let pkt = enc.encode(&Frame { y: &ys, u: &us, v: &vs, y_stride: w, uv_stride: cw });
    (enc.av1c(), pkt.data)
}

fn bx(typ: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut v = ((body.len() + 8) as u32).to_be_bytes().to_vec();
    v.extend_from_slice(typ);
    v.extend_from_slice(body);
    v
}

fn full(typ: &[u8; 4], v: u8, flags: u32, body: &[u8]) -> Vec<u8> {
    let mut b = ((v as u32) << 24 | flags).to_be_bytes().to_vec();
    b.extend_from_slice(body);
    bx(typ, &b)
}

struct It {
    id: u16,
    typ: &'static [u8; 4],
    data: Vec<u8>,
    /// property boxes associated with the item
    props: Vec<Vec<u8>>,
}

fn ispe(w: u32, h: u32) -> Vec<u8> {
    let mut b = w.to_be_bytes().to_vec();
    b.extend_from_slice(&h.to_be_bytes());
    full(b"ispe", 0, 0, &b)
}

/// An AVIF file: items stored in `mdat` (file offsets), `refs` = (type, from, to).
fn avif(primary: u16, items: &[It], refs: &[(&[u8; 4], u16, Vec<u16>)]) -> Vec<u8> {
    let ftyp = bx(b"ftyp", b"avif\0\0\0\0avifmif1miaf");
    let build = |data_start: u32| -> Vec<u8> {
        let mut meta = full(b"hdlr", 0, 0, b"\0\0\0\0pict\0\0\0\0\0\0\0\0\0\0\0\0\0");
        meta.extend(full(b"pitm", 0, 0, &primary.to_be_bytes()));
        let mut iinf = (items.len() as u16).to_be_bytes().to_vec();
        for it in items {
            let mut b = it.id.to_be_bytes().to_vec();
            b.extend_from_slice(&[0, 0]);
            b.extend_from_slice(it.typ);
            b.push(0);
            iinf.extend(full(b"infe", 2, 0, &b));
        }
        meta.extend(full(b"iinf", 0, 0, &iinf));
        let mut iloc = vec![0x44, 0x00];
        iloc.extend_from_slice(&(items.len() as u16).to_be_bytes());
        let mut off = data_start;
        for it in items {
            iloc.extend_from_slice(&it.id.to_be_bytes());
            iloc.extend_from_slice(&[0, 0, 0, 1]); // data ref, one extent
            iloc.extend_from_slice(&off.to_be_bytes());
            iloc.extend_from_slice(&(it.data.len() as u32).to_be_bytes());
            off += it.data.len() as u32;
        }
        meta.extend(full(b"iloc", 0, 0, &iloc));
        if !refs.is_empty() {
            let mut iref = Vec::new();
            for (t, from, to) in refs {
                let mut b = from.to_be_bytes().to_vec();
                b.extend_from_slice(&(to.len() as u16).to_be_bytes());
                for t in to {
                    b.extend_from_slice(&t.to_be_bytes());
                }
                iref.extend(bx(t, &b));
            }
            meta.extend(full(b"iref", 0, 0, &iref));
        }
        let mut ipco = Vec::new();
        let mut ipma = (items.len() as u32).to_be_bytes().to_vec();
        let mut k = 0u8;
        for it in items {
            ipma.extend_from_slice(&it.id.to_be_bytes());
            ipma.push(it.props.len() as u8);
            for p in &it.props {
                ipco.extend_from_slice(p);
                k += 1;
                ipma.push(0x80 | k);
            }
        }
        let mut iprp = bx(b"ipco", &ipco);
        iprp.extend(full(b"ipma", 0, 0, &ipma));
        meta.extend(bx(b"iprp", &iprp));
        full(b"meta", 0, 0, &meta)
    };
    let meta_len = build(0).len() as u32;
    let start = ftyp.len() as u32 + meta_len + 8;
    let mut out = ftyp.clone();
    out.extend(build(start));
    let mdat: Vec<u8> = items.iter().flat_map(|i| i.data.clone()).collect();
    out.extend(bx(b"mdat", &mdat));
    out
}

fn av1c_box(c: &[u8]) -> Vec<u8> {
    bx(b"av1C", c)
}

#[test]
fn colour_and_alpha() {
    let (w, h) = (64usize, 48usize);
    // left half bright (Y 200), right half dark (Y 40); neutral chroma
    let (c, color) = encode(w, h, |x| if x < w / 2 { 200 } else { 40 }, 128, 128);
    // alpha: opaque (limited-range white, 235) left, transparent (16) right
    let (ca, alpha) = encode(w, h, |x| if x < w / 2 { 235 } else { 16 }, 128, 128);
    let auxc = full(b"auxC", 0, 0, b"urn:mpeg:mpegB:cicp:systems:auxiliary:alpha\0");
    let file = avif(
        1,
        &[
            It { id: 1, typ: b"av01", data: color, props: vec![ispe(w as u32, h as u32), av1c_box(&c)] },
            It { id: 2, typ: b"av01", data: alpha, props: vec![ispe(w as u32, h as u32), av1c_box(&ca), auxc] },
        ],
        &[(b"auxl", 2, vec![1])],
    );
    let f = probe_bytes("mem/a.avif", Arc::from(file.clone())).expect("probe");
    assert_eq!(f.kind, FootageKind::Still);
    assert_eq!((f.width, f.height), (64, 48));
    assert_eq!(f.alpha, AlphaMode::Straight);
    assert_eq!(f.codec, "AVIF");
    let pool = MediaPool::new();
    pool.add_bytes("mem/a.avif", Arc::from(file));
    let img = pool.frame_at(&f, Tick::ZERO).expect("decode");
    assert_eq!((img.width, img.height), (64, 48));
    let px = |x: usize, y: usize| img.data[y * w + x];
    let l = px(8, 24);
    let r = px(56, 24);
    // Y 200 limited → (200-16)/219 ≈ 0.84, grey
    assert!((l[0] - 0.84).abs() < 0.03 && (l[0] - l[2]).abs() < 0.03, "left {l:?}");
    assert!((l[3] - 1.0).abs() < 0.02, "left alpha {l:?}");
    assert!(r[3] < 0.02 && r[0] < 0.02, "right (transparent, premultiplied) {r:?}");
}

#[test]
fn grid_of_tiles() {
    let (tw, th) = (32usize, 32usize);
    let (c0, t0) = encode(tw, th, |_| 50, 128, 128);
    let (c1, t1) = encode(tw, th, |_| 220, 128, 128);
    // grid: version 0, flags 0, 1 row, 2 columns, output 60x30 (cropped)
    let grid = vec![0, 0, 0, 1, 0, 60, 0, 30];
    let file = avif(
        3,
        &[
            It { id: 1, typ: b"av01", data: t0, props: vec![ispe(32, 32), av1c_box(&c0)] },
            It { id: 2, typ: b"av01", data: t1, props: vec![ispe(32, 32), av1c_box(&c1)] },
            It { id: 3, typ: b"grid", data: grid, props: vec![ispe(60, 30)] },
        ],
        &[(b"dimg", 3, vec![1, 2])],
    );
    let f = probe_bytes("mem/g.avif", Arc::from(file.clone())).expect("probe");
    assert_eq!((f.width, f.height), (60, 30));
    assert_eq!(f.alpha, AlphaMode::Ignore);
    let pool = MediaPool::new();
    pool.add_bytes("mem/g.avif", Arc::from(file));
    let img = pool.frame_at(&f, Tick::ZERO).expect("decode");
    assert_eq!((img.width, img.height), (60, 30));
    let a = img.data[10 * 60 + 10][0];
    let b = img.data[10 * 60 + 50][0];
    assert!(a < 0.2 && b > 0.85, "tiles {a} {b}");
}

#[test]
fn corrupt_payload_is_an_error() {
    let (c, mut color) = encode(32, 32, |_| 100, 128, 128);
    let n = color.len();
    color.truncate(n / 2);
    let file = avif(1, &[It { id: 1, typ: b"av01", data: color, props: vec![ispe(32, 32), av1c_box(&c)] }], &[]);
    let f = probe_bytes("mem/c.avif", Arc::from(file.clone())).expect("probe reads the container");
    let pool = MediaPool::new();
    pool.add_bytes("mem/c.avif", Arc::from(file));
    assert!(pool.frame_at(&f, Tick::ZERO).is_err());
}
