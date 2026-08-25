//! Turning a file on disk into pixels the AI pipeline can read.
//!
//! Two rules from §19 shape this module: the original file is never written
//! to, and inference runs on a downscaled copy rather than a 45-megapixel
//! original.

use std::path::Path;

use image::{imageops::FilterType, DynamicImage, RgbImage};

use crate::ffmpeg::Ffmpeg;
use crate::formats::{self, Decoder, MediaKind};
use crate::metadata::Orientation;
use crate::raw;
use crate::{MediaError, Result};

/// Loads a still image, applying its EXIF orientation and capping the longest
/// edge at `max_dim` if given.
pub fn load_image(path: &Path, orientation: u16, max_dim: Option<u32>, ffmpeg: Option<&Ffmpeg>) -> Result<RgbImage> {
    let (kind, decoder) = formats::classify(path)
        .ok_or_else(|| MediaError::Unsupported(path.display().to_string()))?;
    if kind != MediaKind::Photo {
        return Err(MediaError::Unsupported(format!("{} is not a still image", path.display())));
    }

    let image = match decoder {
        Decoder::Native => {
            let img = image::open(path).map_err(|e| MediaError::Decode(format!("{}: {e}", path.display())))?;
            let img = match max_dim {
                Some(max) if img.width().max(img.height()) > max => resize_within(&img, max),
                _ => img,
            };
            img.to_rgb8()
        }
        Decoder::Ffmpeg => decode_raw_or_ffmpeg(path, max_dim, ffmpeg)?,
    };

    Ok(apply_orientation(image, Orientation::from_exif(orientation)))
}

/// Decodes a file FFmpeg is nominally responsible for, preferring the JPEG a
/// camera embedded in it.
///
/// The order is deliberate. A useful preview wins outright: it is the camera's
/// own rendering, it costs a seek rather than a demosaic, and for RAF it is the
/// only thing that works at all. A preview too small to be the picture yields to
/// FFmpeg — but is still better than nothing, so it is kept in reserve for when
/// FFmpeg cannot open the file either. HEIC and AVIF never reach the preview
/// path at all; they are finished images FFmpeg decodes properly.
fn decode_raw_or_ffmpeg(path: &Path, max_dim: Option<u32>, ffmpeg: Option<&Ffmpeg>) -> Result<RgbImage> {
    let preview = if raw::is_raw(path) { raw::best_preview(path) } else { None };

    if let Some(preview) = preview.as_ref().filter(|p| p.is_useful()) {
        tracing::debug!(
            file = %path.display(),
            width = preview.width,
            height = preview.height,
            "decoding the embedded preview instead of the sensor data"
        );
        return decode_preview(path, preview, max_dim);
    }

    let ffmpeg_error = match ffmpeg {
        Some(ff) => match ff.decode_still(path, max_dim) {
            Ok(image) => return Ok(image),
            Err(e) => e,
        },
        None => MediaError::MissingFfmpeg(format!(
            "{} needs FFmpeg to decode",
            formats::extension(path).to_uppercase()
        )),
    };

    match preview.as_ref() {
        Some(preview) => {
            tracing::debug!(
                file = %path.display(),
                width = preview.width,
                height = preview.height,
                "FFmpeg could not decode this file; using its small embedded preview"
            );
            decode_preview(path, preview, max_dim)
        }
        None => Err(ffmpeg_error),
    }
}

fn decode_preview(path: &Path, preview: &raw::Preview, max_dim: Option<u32>) -> Result<RgbImage> {
    debug_assert!(preview.complete, "a truncated preview is for indexing, not decoding");
    let image = image::load_from_memory(&preview.bytes)
        .map_err(|e| MediaError::Decode(format!("{}: embedded preview: {e}", path.display())))?;
    let image = match max_dim {
        Some(max) if image.width().max(image.height()) > max => resize_within(&image, max),
        _ => image,
    };
    Ok(image.to_rgb8())
}

/// Grabs one frame from a video, already oriented and downscaled.
pub fn load_video_frame(
    path: &Path,
    timestamp: f64,
    orientation: u16,
    max_dim: Option<u32>,
    ffmpeg: &Ffmpeg,
) -> Result<RgbImage> {
    let frame = ffmpeg.extract_frame(path, timestamp, max_dim)?;
    Ok(apply_orientation(frame, Orientation::from_exif(orientation)))
}

fn resize_within(img: &DynamicImage, max: u32) -> DynamicImage {
    // Lanczos3 costs a little more than Triangle but keeps small faces sharp
    // enough for the detector to find them after a heavy downscale.
    img.resize(max, max, FilterType::Lanczos3)
}

/// Rotates and flips pixels so the subject is upright. Doing this once, here,
/// means the detector, the thumbnail and the stored bounding boxes all share
/// one coordinate system.
pub fn apply_orientation(image: RgbImage, orientation: Orientation) -> RgbImage {
    use image::imageops::{flip_horizontal, flip_vertical, rotate180, rotate270, rotate90};
    match orientation {
        Orientation::Normal => image,
        Orientation::FlipHorizontal => flip_horizontal(&image),
        Orientation::Rotate180 => rotate180(&image),
        Orientation::FlipVertical => flip_vertical(&image),
        Orientation::Transpose => rotate90(&flip_horizontal(&image)),
        Orientation::Rotate90 => rotate90(&image),
        Orientation::Transverse => rotate270(&flip_horizontal(&image)),
        Orientation::Rotate270 => rotate270(&image),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgb;

    fn probe_image() -> RgbImage {
        // A 2x1 image: left pixel red, right pixel blue.
        let mut img = RgbImage::new(2, 1);
        img.put_pixel(0, 0, Rgb([255, 0, 0]));
        img.put_pixel(1, 0, Rgb([0, 0, 255]));
        img
    }

    #[test]
    fn normal_orientation_is_a_no_op() {
        let img = apply_orientation(probe_image(), Orientation::Normal);
        assert_eq!(img.dimensions(), (2, 1));
        assert_eq!(img.get_pixel(0, 0), &Rgb([255, 0, 0]));
    }

    #[test]
    fn rotate90_swaps_axes_and_moves_pixels() {
        let img = apply_orientation(probe_image(), Orientation::Rotate90);
        assert_eq!(img.dimensions(), (1, 2));
        // Rotating clockwise sends the left (red) pixel to the top.
        assert_eq!(img.get_pixel(0, 0), &Rgb([255, 0, 0]));
        assert_eq!(img.get_pixel(0, 1), &Rgb([0, 0, 255]));
    }

    #[test]
    fn horizontal_flip_reverses_columns() {
        let img = apply_orientation(probe_image(), Orientation::FlipHorizontal);
        assert_eq!(img.get_pixel(0, 0), &Rgb([0, 0, 255]));
        assert_eq!(img.get_pixel(1, 0), &Rgb([255, 0, 0]));
    }
}
