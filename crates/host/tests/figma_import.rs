//! Figma import through the fully wired session: what has no native counterpart comes in as
//! PDF footage (continuously rasterised, transparent outside the artwork), PNG when the PDF fails.

use effectcraft_engine::render::RenderOpts;
use effectcraft_project::{ItemId, ItemKind, LayerSource};
use serde_json::{Value, json};

fn tmp(name: &str) -> String {
    let d = std::env::temp_dir().join(format!("effectcraft-host-figma-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d.join(name).to_string_lossy().to_string()
}

/// A 50 × 40 page with a red rectangle over its left half only (the rest is transparent).
fn half_red_pdf() -> Vec<u8> {
    let content = "1 0 0 rg 0 0 25 40 re f\n";
    effectcraft_pdf::write::pdf(
        &[
            (1, "<< /Type /Catalog /Pages 2 0 R >>".into(), None),
            (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(), None),
            (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 50 40] /Contents 4 0 R >>".into(), None),
            (4, "<< >>".into(), Some(content.as_bytes().to_vec())),
        ],
        1,
    )
}

fn red_png() -> Vec<u8> {
    let img = image::RgbaImage::from_pixel(100, 80, image::Rgba([255, 0, 0, 255]));
    let mut out = std::io::Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png).unwrap();
    out.into_inner()
}

/// A 200 × 100 blue frame with an image-filled 50 × 40 rectangle at (20, 30). Each test uses its
/// own `key`: the images land in one folder per key, and the tests run in parallel.
fn payload(key: &str, images: Value) -> Value {
    json!({"fileKey": key, "images": images, "nodes": [{
        "id": "1:1", "name": "Frame", "type": "FRAME", "absoluteTransform": [[1, 0, 0], [0, 1, 0]], "size": {"x": 200, "y": 100},
        "absoluteBoundingBox": {"x": 0, "y": 0, "width": 200, "height": 100},
        "fills": [{"type": "SOLID", "color": {"r": 0, "g": 0, "b": 1, "a": 1}}],
        "children": [{
            "id": "1:2", "name": "Photo", "type": "RECTANGLE", "absoluteTransform": [[1, 0, 20], [0, 1, 30]], "size": {"x": 50, "y": 40},
            "absoluteBoundingBox": {"x": 20, "y": 30, "width": 50, "height": 40},
            "fills": [{"type": "IMAGE", "scaleMode": "FILL"}]
        }]
    }]})
}

fn pixel(s: &effectcraft_engine::Session, cid: ItemId, x: usize, y: usize) -> [f32; 4] {
    let img = s.render(cid, effectcraft_time::Tick::ZERO, RenderOpts::default());
    img.data[y * img.width as usize + x]
}

#[test]
fn image_fills_come_in_as_sharp_transparent_pdf_footage() {
    let pdf = tmp("photo.pdf");
    std::fs::write(&pdf, half_red_pdf()).unwrap();
    let mut s = effectcraft_host::session();
    let r = s.execute_checked("file.importFigma", json!({"payload": payload("FixturePdf", json!({"1:2": {"pdfPath": pdf}}))})).unwrap();
    assert_eq!(r["rasterized"][0]["format"], "pdf", "{r}");
    let cid = ItemId(r["comp"].as_u64().unwrap());
    let comp = s.project.comp(cid).unwrap();
    let layer = comp.layers.iter().find(|l| l.name == "Photo").unwrap();
    assert!(layer.switches.collapse, "PDF footage is continuously rasterised");
    let LayerSource::Footage { item } = layer.source else { panic!("not footage") };
    let Some(ItemKind::Footage(f)) = s.project.item(item).map(|i| &i.kind) else { panic!() };
    assert_eq!(f.codec, "PDF");
    // Left half red, right half transparent over the blue frame (no white page).
    let red = pixel(&s, cid, 30, 50);
    let blue = pixel(&s, cid, 60, 50);
    assert!(red[0] > 0.9 && red[2] < 0.1, "{red:?}");
    assert!(blue[2] > 0.9 && blue[0] < 0.1, "{blue:?}");

    // At 200 % the edge stays a hard edge (vector, not upscaled pixels).
    let lid = layer.id.0;
    s.execute_checked("prop.set", json!({"layer": lid, "path": "transform/scale", "value": [200, 200]})).unwrap();
    let img = s.render(cid, effectcraft_time::Tick::ZERO, RenderOpts::default());
    let w = img.width as usize;
    let row: Vec<f32> = (0..200).map(|x| img.data[50 * w + x][0]).collect();
    let soft = row.iter().filter(|v| **v > 0.05 && **v < 0.95).count();
    assert!(soft <= 2, "edge pixels at 200 %: {soft}");
}

#[test]
fn a_broken_pdf_falls_back_to_png() {
    let bad = tmp("broken.pdf");
    std::fs::write(&bad, b"%PDF-1.7\nnot really a pdf").unwrap();
    let png = tmp("photo.png");
    std::fs::write(&png, red_png()).unwrap();
    let mut s = effectcraft_host::session();
    let r = s.execute_checked("file.importFigma", json!({"payload": payload("FixtureFallback", json!({"1:2": {"pdfPath": bad, "path": png}}))})).unwrap();
    assert_eq!(r["rasterized"][0]["format"], "png", "{r}");
    let notes = r["notes"].to_string();
    assert!(notes.contains("PNG used"), "{notes}");
    let cid = ItemId(r["comp"].as_u64().unwrap());
    let layer = s.project.comp(cid).unwrap().layers.iter().find(|l| l.name == "Photo").unwrap();
    assert!(!layer.switches.collapse);
    let red = pixel(&s, cid, 60, 50);
    assert!(red[0] > 0.9, "{red:?}");
}
