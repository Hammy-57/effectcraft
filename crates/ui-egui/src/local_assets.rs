//! Optional owner-local artwork. Missing or malformed assets retain the built-in UI.
use egui::{Color32, Context, Painter, Rect, TextureHandle};
use std::io::Read;

fn read_image(name: &str) -> Option<image::RgbaImage> {
    let root = std::path::PathBuf::from(std::env::var_os("EFFECTCRAFT_ICON_REFERENCE")?);
    let path = root.parent()?.join("extra").join(format!("{name}.png"));
    let mut bytes = Vec::new();
    std::fs::File::open(path).ok()?.take(262145).read_to_end(&mut bytes).ok()?;
    if bytes.len() > 262144 {
        return None;
    }
    let reader = image::ImageReader::new(std::io::Cursor::new(&bytes)).with_guessed_format().ok()?;
    let (w, h) = reader.into_dimensions().ok()?;
    if w == 0 || h == 0 || w > 256 || h > 256 {
        return None;
    }
    Some(image::load_from_memory(&bytes).ok()?.to_rgba8())
}

pub(crate) fn icon(ctx: &Context, name: &str) -> Option<TextureHandle> {
    let id = egui::Id::new(("local-icon", name));
    if let Some(cached) = ctx.data(|d| d.get_temp::<Option<TextureHandle>>(id)) {
        return cached;
    }
    let texture = read_image(name).map(|image| {
        let pixels = image.pixels().map(|p| Color32::from_white_alpha(p.0[3])).collect();
        ctx.load_texture(name, egui::ColorImage::new([image.width() as usize, image.height() as usize], pixels), egui::TextureOptions::LINEAR)
    });
    ctx.data_mut(|d| d.insert_temp(id, texture.clone()));
    texture
}

pub(crate) fn paint(painter: &Painter, rect: Rect, name: &str, color: Color32) -> bool {
    let Some(texture) = icon(painter.ctx(), name) else { return false };
    painter.image(texture.id(), rect, Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)), color);
    true
}

pub(crate) fn cursor(ctx: &Context, name: &str, hotspot: [u16; 2]) {
    let scale = if ctx.pixels_per_point() >= 1.5 { 2 } else { 1 };
    let id = egui::Id::new(("local-cursor", name, scale));
    let cached = ctx.data(|d| d.get_temp::<Option<egui::CustomCursorImage>>(id));
    let cursor = cached.unwrap_or_else(|| {
        let image = read_image(&format!("cursor-{name}{}", if scale == 2 { "@2x" } else { "" }));
        let cursor = image.and_then(|image| {
            let size = [image.width() as u16, image.height() as u16];
            let hotspot = [hotspot[0] * scale, hotspot[1] * scale];
            if hotspot[0] >= size[0] || hotspot[1] >= size[1] {
                return None;
            }
            Some(egui::CustomCursorImage { size, hotspot, rgba: image.into_raw().into() })
        });
        ctx.data_mut(|d| d.insert_temp(id, cursor.clone()));
        cursor
    });
    if let Some(cursor) = cursor {
        ctx.set_cursor_image(Some(cursor));
    }
}
