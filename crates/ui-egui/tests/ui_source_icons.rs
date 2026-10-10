//! The icon before a layer's name (Timeline) and an item's name (Project panel) follows After
//! Effects: layer-type glyphs, the source item's icon, and a colour swatch for solids, adjustment
//! layers and nulls (never the Adjustment / 3D *switch* icons).
use effectcraft_engine::project::ItemId;
use effectcraft_ui_egui::icons::{Icon, SourceIcon};
use effectcraft_ui_egui::panels::{project::item_icon, timeline::layer_icon};
use egui::Color32;
use serde_json::json;

#[test]
fn layer_and_item_icons_match_after_effects() {
    let png = std::env::temp_dir().join(format!("ec-source-icons-{}.png", std::process::id()));
    image::RgbaImage::from_pixel(4, 4, image::Rgba([200, 40, 40, 255])).save(&png).unwrap();

    let mut s = effectcraft_host::session();
    let still = s.execute("file.import", json!({"paths": [png.to_string_lossy()]})).unwrap()["items"][0].as_u64().unwrap();
    let inner = s.execute("comp.new", json!({"name": "Inner", "width": 64, "height": 36, "duration": 1})).unwrap()["comp"].as_u64().unwrap();
    s.execute("comp.new", json!({"name": "Main", "width": 64, "height": 36, "duration": 1})).unwrap();
    let mut add = |cmd: &str, p: serde_json::Value| s.execute(cmd, p).unwrap()["layer"].as_u64().unwrap();
    let ids = [
        add("layer.newText", json!({"text": "Title"})),
        add("layer.newShape", json!({"kind": "rect", "size": [10, 10]})),
        add("layer.newNull", json!({})),
        add("layer.newSolid", json!({"name": "Red", "color": "#ff0000"})),
        add("layer.newAdjustment", json!({"name": "Grade"})),
        add("layer.newCamera", json!({})),
        add("layer.newLight", json!({})),
        add("layer.newPrimitive", json!({"kind": "sphere"})),
        add("layer.addItem", json!({"item": inner})),
        add("layer.addItem", json!({"item": still})),
    ];
    let _ = std::fs::remove_file(&png);

    let comp = s.active_comp().unwrap();
    let icon = |id: u64| layer_icon(comp.layers.iter().find(|l| l.id.0 == id).unwrap(), &s.project);
    let g = SourceIcon::Glyph;
    assert_eq!(icon(ids[0]), g(Icon::TextLayer));
    assert_eq!(icon(ids[1]), g(Icon::ShapeLayer));
    assert_eq!(icon(ids[2]), SourceIcon::Swatch(Color32::WHITE), "nulls show a white swatch");
    assert_eq!(icon(ids[3]), SourceIcon::Swatch(Color32::from_rgb(255, 0, 0)), "solids show their colour");
    assert!(matches!(icon(ids[4]), SourceIcon::Swatch(_)), "adjustment layers show their solid's swatch, not the switch icon");
    assert_eq!(icon(ids[5]), g(Icon::Camera));
    assert_eq!(icon(ids[6]), g(Icon::Light));
    assert_eq!(icon(ids[7]), g(Icon::Primitive));
    assert_eq!(icon(ids[8]), g(Icon::Comp));
    assert_eq!(icon(ids[9]), g(Icon::Image));

    let item = |id: u64| item_icon(s.project.item(ItemId(id)).unwrap());
    assert_eq!(item(inner), g(Icon::Comp));
    assert_eq!(item(still), g(Icon::Image));
    let solid = comp.layers.iter().find(|l| l.id.0 == ids[3]).and_then(|l| l.source.item()).unwrap();
    assert_eq!(item(solid.0), SourceIcon::Swatch(Color32::from_rgb(255, 0, 0)));
}
