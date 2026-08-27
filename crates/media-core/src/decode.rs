//! Turning a file on disk into pixels the AI pipeline can read.
//!
//! Two rules from §19 shape this module: the original file is never written
//! to, and inference runs on a downscaled copy rather than a 45-megapixel
//! original.

use std::path::Path;
use std::time::Instant;

use image::{imageops::FilterType, DynamicImage, RgbImage};

use crate::ffmpeg::Ffmpeg;
use crate::formats::{self, Decoder, MediaKind};
use crate::metadata::Orientation;
use crate::raw;
use crate::{MediaError, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeMethod {
    Native,
    FfmpegStill,
    LibRawEmbeddedPreview,
    LibRawHalfSizeDemosaic,
}

impl DecodeMethod {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::FfmpegStill => "ffmpeg-still",
            Self::LibRawEmbeddedPreview => "libraw-embedded-preview",
            Self::LibRawHalfSizeDemosaic => "libraw-half-size-demosaic",
        }
    }
}

/// Normalised output shared by thumbnails, full-image serving and AI.
pub struct DecodedImage {
    pub image: RgbImage,
    pub source_format: String,
    pub decode_method: DecodeMethod,
    pub timings: raw::DecodeTimings,
}

/// Loads a still image, applying its EXIF orientation and capping the longest
/// edge at `max_dim` if given.
pub fn load_image(path: &Path, orientation: u16, max_dim: Option<u32>, ffmpeg: Option<&Ffmpeg>) -> Result<RgbImage> {
    Ok(decode_image(path, orientation, max_dim, ffmpeg)?.image)
}

pub fn decode_image(
    path: &Path,
    orientation: u16,
    max_dim: Option<u32>,
    ffmpeg: Option<&Ffmpeg>,
) -> Result<DecodedImage> {
    let total_started = Instant::now();
    let (kind, decoder) = formats::classify(path).ok_or_else(|| MediaError::Unsupported(path.display().to_string()))?;
    if kind != MediaKind::Photo {
        return Err(MediaError::Unsupported(format!(
            "{} is not a still image",
            path.display()
        )));
    }

    let source_format = formats::extension(path).to_ascii_uppercase();
    let (image, decode_method, mut timings) = match decoder {
        Decoder::Native => {
            let opened = Instant::now();
            let img = image::open(path).map_err(|e| MediaError::Decode(format!("{}: {e}", path.display())))?;
            let mut timings = raw::DecodeTimings {
                open: opened.elapsed(),
                ..Default::default()
            };
            let img = match max_dim {
                Some(max) if img.width().max(img.height()) > max => {
                    let resized = Instant::now();
                    let result = resize_within(&img, max);
                    timings.resize = resized.elapsed();
                    result
                }
                _ => img,
            };
            (img.to_rgb8(), DecodeMethod::Native, timings)
        }
        Decoder::LibRaw => {
            let decoded = raw::decode(path, max_dim)?;
            let method = match decoded.method {
                raw::DecodeMethod::EmbeddedPreview => DecodeMethod::LibRawEmbeddedPreview,
                raw::DecodeMethod::HalfSizeDemosaic => DecodeMethod::LibRawHalfSizeDemosaic,
            };
            (decoded.image, method, decoded.timings)
        }
        Decoder::Ffmpeg => {
            let ff = ffmpeg.ok_or_else(|| {
                MediaError::MissingFfmpeg(format!(
                    "{} needs FFmpeg to decode",
                    formats::extension(path).to_uppercase()
                ))
            })?;
            let decoded = Instant::now();
            let image = ff.decode_still(path, max_dim)?;
            (
                image,
                DecodeMethod::FfmpegStill,
                raw::DecodeTimings {
                    full_decode: decoded.elapsed(),
                    ..Default::default()
                },
            )
        }
    };

    let image = apply_orientation(image, Orientation::from_exif(orientation));
    timings.total = total_started.elapsed();
    Ok(DecodedImage {
        image,
        source_format,
        decode_method,
        timings,
    })
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
