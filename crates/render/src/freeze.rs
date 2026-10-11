//! Preview-only frozen pixels in the compositor's blending space. Exporters leave the
//! renderer's freeze source unset. A cache miss must render the original scene.

use std::io::{Read, Write};
use std::path::Path;
use std::sync::Arc;

use effectcraft_project::{ItemId, LayerId};
use effectcraft_time::Tick;

use crate::{Image, RenderOpts};

/// The engine supplies only accepted versions; freshness decisions never happen here.
pub trait FreezeSource: Send + Sync {
    fn frame(&self, comp: ItemId, layers: &[LayerId], time: Tick, opts: &RenderOpts) -> Option<Arc<Image>>;
    /// Selected layer IDs in bottom-to-top order, beginning at `bottom`.
    fn selection(&self, comp: ItemId, bottom: LayerId) -> Option<&[LayerId]>;
}

pub(crate) fn selection_end(visible: &[&effectcraft_project::Layer], ids: &[LayerId], start: usize) -> Option<usize> {
    if visible.get(..start)?.iter().any(|l| ids.contains(&l.id)) {
        return None;
    }
    let count = visible.iter().filter(|l| ids.contains(&l.id)).count();
    let end = start.checked_add(count)?;
    (count > 0 && visible.get(start..end)?.iter().all(|l| ids.contains(&l.id))).then_some(end)
}

const MAGIC: &[u8; 8] = b"ECFRZ002";
pub const MAX_PIXELS: usize = 33_554_432;
const HEADER_BYTES: usize = 32;

fn invalid(message: &str) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, message)
}

fn pixel_count(width: u32, height: u32) -> std::io::Result<usize> {
    let count = u64::from(width)
        .checked_mul(u64::from(height))
        .filter(|n| *n > 0 && *n <= MAX_PIXELS as u64)
        .ok_or_else(|| invalid("Smart Freeze frame dimensions exceed the pixel budget"))?;
    usize::try_from(count).map_err(|_| invalid("Smart Freeze frame does not fit this machine"))
}

/// Immutable, checksummed float cache; compression preserves HDR and alpha exactly.
pub fn write_frame(path: &Path, image: &Image) -> std::io::Result<u64> {
    let count = pixel_count(image.width, image.height)?;
    if image.data.len() != count {
        return Err(invalid("Smart Freeze pixel count does not match dimensions"));
    }
    let mut raw = Vec::new();
    raw.try_reserve_exact(count.saturating_mul(16)).map_err(|_| invalid("not enough memory to encode Smart Freeze frame"))?;
    for pixel in &image.data {
        for channel in pixel {
            if !channel.is_finite() {
                return Err(invalid("Smart Freeze frame contains non-finite pixels"));
            }
            raw.extend_from_slice(&channel.to_le_bytes());
        }
    }
    let compressed = lz4_flex::compress(&raw);
    let mut hash = crate::disk_cache::Hash128::default();
    hash.write(&image.width.to_le_bytes());
    hash.write(&image.height.to_le_bytes());
    hash.write(&compressed);
    let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(MAGIC)?;
    file.write_all(&image.width.to_le_bytes())?;
    file.write_all(&image.height.to_le_bytes())?;
    file.write_all(&hash.finish().to_le_bytes())?;
    file.write_all(&compressed)?;
    file.sync_all()?;
    Ok((HEADER_BYTES as u64).saturating_add(compressed.len() as u64))
}

pub fn read_frame(path: &Path) -> std::io::Result<Image> {
    let mut file = std::fs::File::open(path)?;
    let mut magic = [0u8; 8];
    let mut w = [0u8; 4];
    let mut h = [0u8; 4];
    let mut checksum = [0u8; 16];
    file.read_exact(&mut magic)?;
    if &magic != MAGIC {
        return Err(invalid("unrecognized Smart Freeze frame"));
    }
    file.read_exact(&mut w)?;
    file.read_exact(&mut h)?;
    file.read_exact(&mut checksum)?;
    let (width, height) = (u32::from_le_bytes(w), u32::from_le_bytes(h));
    let count = pixel_count(width, height)?;
    let raw_len = count.checked_mul(16).ok_or_else(|| invalid("Smart Freeze frame byte count overflow"))?;
    let limit = lz4_flex::block::get_maximum_output_size(raw_len);
    let mut packed = Vec::new();
    file.take(limit.saturating_add(1) as u64).read_to_end(&mut packed)?;
    if packed.len() > limit {
        return Err(invalid("Smart Freeze frame exceeds its declared dimensions"));
    }
    let mut hash = crate::disk_cache::Hash128::default();
    hash.write(&w);
    hash.write(&h);
    hash.write(&packed);
    if hash.finish() != u128::from_le_bytes(checksum) {
        return Err(invalid("Smart Freeze checksum mismatch"));
    }
    let mut raw = Vec::new();
    raw.try_reserve_exact(raw_len).map_err(|_| invalid("not enough memory to decode Smart Freeze frame"))?;
    raw.resize(raw_len, 0);
    let actual = lz4_flex::decompress_into(&packed, &mut raw).map_err(|_| invalid("corrupt Smart Freeze compression"))?;
    if actual != raw_len {
        return Err(invalid("truncated Smart Freeze pixels"));
    }
    let mut data = Vec::new();
    data.try_reserve_exact(count).map_err(|_| invalid("not enough memory for Smart Freeze pixels"))?;
    for pixel in raw.as_chunks::<16>().0 {
        let mut rgba = [0.0f32; 4];
        for (channel, bytes) in rgba.iter_mut().zip(pixel.as_chunks::<4>().0) {
            *channel = f32::from_le_bytes(*bytes);
            if !channel.is_finite() {
                return Err(invalid("Smart Freeze frame contains non-finite pixels"));
            }
        }
        data.push(rgba);
    }
    Ok(Image { width, height, data })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn float_cache_preserves_hdr_alpha_and_refuses_overwrite() {
        let path = std::env::temp_dir().join(format!(
            "ec-freeze-{}-{}.frame",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        let image = Image { width: 2, height: 1, data: vec![[1.5, -0.2, 0.3, 0.5], [0.0; 4]] };
        write_frame(&path, &image).unwrap();
        let restored = read_frame(&path).unwrap();
        assert_eq!(restored.data, image.data);
        assert!(write_frame(&path, &image).is_err());
        std::fs::write(&path, b"ECFRZ002\xff\xff\xff\xff\xff\xff\xff\xff________________").unwrap();
        assert!(read_frame(&path).unwrap_err().to_string().contains("pixel budget"));
        std::fs::remove_file(path).unwrap();
    }
}
