//! GIF89a animation through image's public animation API. Frames include disposal/compositing.
use std::io::Cursor;

use effectcraft_project::{Footage, FootageKind};
use effectcraft_time::{FrameRate, Tick};
use image::{AnimationDecoder, ImageDecoder};

use crate::{MediaError, Result};

const MAX_FRAMES: usize = 100_000;

fn decoder(bytes: &[u8]) -> Result<image::codecs::gif::GifDecoder<Cursor<&[u8]>>> {
    let mut d = image::codecs::gif::GifDecoder::new(Cursor::new(bytes)).map_err(error)?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(16_384);
    limits.max_image_height = Some(16_384);
    limits.max_alloc = Some(256 << 20);
    d.set_limits(limits).map_err(error)?;
    Ok(d)
}

fn error(e: image::ImageError) -> MediaError {
    MediaError::Decode(format!("GIF: {e}"))
}

fn delay(frame: &image::Frame) -> Tick {
    let (num, den) = frame.delay().numer_denom_ms();
    // GIF delays are centiseconds. A zero delay must still advance the source clock.
    Tick::from_seconds_f64((f64::from(num) / f64::from(den.max(1)) / 1000.0).max(0.01))
}

pub(crate) fn probe(path: &str, bytes: &[u8]) -> Result<Footage> {
    let d = decoder(bytes)?;
    let (w, h) = d.dimensions();
    let mut duration = Tick::ZERO;
    let mut count = 0;
    for f in d.into_frames() {
        count += 1;
        if count > MAX_FRAMES {
            return Err(MediaError::Unsupported("GIF exceeds 100000 frames".into()));
        }
        duration.0 = duration.0.saturating_add(delay(&f.map_err(error)?).0);
    }
    if count == 0 {
        return Err(MediaError::Decode("GIF contains no frames".into()));
    }
    let mut f = crate::probe::still_footage(path, w, h, image::ImageFormat::Gif, true);
    if count > 1 {
        f.kind = FootageKind::Video;
        f.duration = duration;
        // Use the format's clock rather than averaging variable frame delays.
        f.frame_rate = FrameRate::new(100, 1);
    }
    Ok(f)
}

// shortcut: uncached seeks replay GIF disposal from the start; retain decoder state if profiling warrants it.
pub(crate) fn frame(bytes: &[u8], t: Tick) -> Result<image::DynamicImage> {
    let mut end = Tick::ZERO;
    let mut last = None;
    for (i, f) in decoder(bytes)?.into_frames().enumerate() {
        if i >= MAX_FRAMES {
            return Err(MediaError::Unsupported("GIF exceeds 100000 frames".into()));
        }
        let f = f.map_err(error)?;
        end.0 = end.0.saturating_add(delay(&f).0);
        let img = image::DynamicImage::ImageRgba8(f.into_buffer());
        if t < end {
            return Ok(img);
        }
        last = Some(img);
    }
    last.ok_or_else(|| MediaError::Decode("GIF contains no frames".into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn animation(delays: &[u32]) -> Arc<[u8]> {
        let mut bytes = Vec::new();
        {
            let mut enc = image::codecs::gif::GifEncoder::new(&mut bytes);
            for (i, ms) in delays.iter().enumerate() {
                let color = if i == 0 { [255, 0, 0, 255] } else { [0, 255, 0, 255] };
                let buffer = image::RgbaImage::from_pixel(2, 2, image::Rgba(color));
                enc.encode_frame(image::Frame::from_parts(buffer, 0, 0, image::Delay::from_numer_denom_ms(*ms, 1))).unwrap();
            }
        }
        bytes.into()
    }

    #[test]
    fn animated_gif_uses_delays_in_cached_preview_and_export_frames() {
        let bytes = animation(&[100, 300]);
        let mut f = crate::probe_bytes("animation.gif", bytes.clone()).unwrap();
        assert_eq!(f.kind, FootageKind::Video);
        assert_eq!(f.duration, Tick::from_seconds_f64(0.4));
        let pool = crate::MediaPool::new();
        pool.add_bytes(&f.path, bytes);
        for (seconds, color) in [(0.0, 0), (0.099, 0), (0.1, 1), (0.35, 1), (4.0, 1), (0.0, 0)] {
            let image = pool.frame_at(&f, Tick::from_seconds_f64(seconds)).unwrap();
            assert!(image.data.iter().all(|p| p[color] == 1.0 && p[1 - color] == 0.0));
        }
        f.loop_count = 2;
        let image = pool.frame_at(&f, Tick::from_seconds_f64(0.4)).unwrap();
        assert_eq!(image.data[0], [1.0, 0.0, 0.0, 1.0]);
        f.native_rate = Some(f.frame_rate);
        f.frame_rate = FrameRate::new(50, 1);
        assert_eq!(pool.frame_at(&f, Tick::from_seconds_f64(0.2)).unwrap().data[0], [0.0, 1.0, 0.0, 1.0]);
    }

    #[test]
    fn single_frame_gif_stays_a_still_and_zero_delays_advance() {
        assert_eq!(probe("still.gif", &animation(&[100])).unwrap().kind, FootageKind::Still);
        assert_eq!(probe("zero.gif", &animation(&[0, 0])).unwrap().duration, Tick::from_seconds_f64(0.02));
        assert!(probe("bad.gif", b"GIF89a").is_err());
    }
}
