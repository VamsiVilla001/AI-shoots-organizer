//! Preview-first camera RAW decoding.
//!
//! Windows uses LibRaw, while macOS uses Rawler because rawlib's packaged Unix
//! archive is linked against Linux's C++ runtime. Both paths first use the
//! camera's embedded preview and fall back to demosaicing. Pixels stay in
//! memory; source files are never modified.

use std::fmt;
use std::path::Path;
use std::time::{Duration, Instant};

use image::{DynamicImage, RgbImage};
#[cfg(not(target_os = "macos"))]
use rawlib::{DecodeOptions, ImageFormat, RawProcessor, ThumbnailData};

use crate::{MediaError, Result};

/// Absolute floor for an embedded preview. The requested decode size can raise
/// this requirement so RAW analysis receives the same working resolution as a
/// JPEG/PNG analysis instead of silently accepting a much smaller camera
/// thumbnail.
const MIN_PREVIEW_LONG_EDGE: u32 = 640;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RawErrorCode {
    Unsupported,
    Corrupt,
    PreviewUnavailable,
    DecodeFailed,
    OutOfMemory,
}

impl fmt::Display for RawErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Unsupported => "RAW_UNSUPPORTED",
            Self::Corrupt => "RAW_CORRUPT",
            Self::PreviewUnavailable => "RAW_PREVIEW_UNAVAILABLE",
            Self::DecodeFailed => "RAW_DECODE_FAILED",
            Self::OutOfMemory => "RAW_OUT_OF_MEMORY",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeMethod {
    EmbeddedPreview,
    HalfSizeDemosaic,
}

impl DecodeMethod {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::EmbeddedPreview => "raw-embedded-preview",
            Self::HalfSizeDemosaic => "raw-demosaic-fallback",
        }
    }
}

const RAW_DECODER: &str = if cfg!(target_os = "macos") { "rawler" } else { "libraw" };

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DecodeTimings {
    pub open: Duration,
    pub preview: Duration,
    pub full_decode: Duration,
    pub resize: Duration,
    pub total: Duration,
}

pub struct DecodedRaw {
    pub image: RgbImage,
    pub source_format: String,
    pub method: DecodeMethod,
    pub timings: DecodeTimings,
}

/// Opens a RAW, first trying its largest embedded preview and then falling back
/// to a demosaic. The LibRaw fallback is half-size; Rawler develops to sRGB and
/// the common resize step immediately bounds the result.
pub fn decode(path: &Path, max_dim: Option<u32>) -> Result<DecodedRaw> {
    let total_started = Instant::now();
    let source_format = crate::formats::extension(path).to_ascii_uppercase();
    let mut timings = DecodeTimings::default();

    let preview_started = Instant::now();
    let preview_result = decode_preview(path, &mut timings);
    timings.preview = preview_started.elapsed();

    let preview_detail = match preview_result {
        Ok(mut image) => {
            let preview_long_edge = image.width().max(image.height());
            let required_long_edge = required_preview_long_edge(max_dim);
            if preview_long_edge >= required_long_edge {
                resize_if_needed(&mut image, max_dim, &mut timings);
                timings.total = total_started.elapsed();
                log_success(path, &source_format, DecodeMethod::EmbeddedPreview, &image, timings);
                return Ok(DecodedRaw {
                    image,
                    source_format,
                    method: DecodeMethod::EmbeddedPreview,
                    timings,
                });
            }
            format!("embedded preview was {preview_long_edge}px; this operation needs at least {required_long_edge}px")
        }
        Err(error) => error,
    };

    let full_started = Instant::now();
    let full_result = decode_fallback(path, &mut timings);
    timings.full_decode = full_started.elapsed();
    let mut image = match full_result {
        Ok(image) => image,
        Err(error) => {
            timings.total = total_started.elapsed();
            let mapped = raw_error(path, error, RawErrorCode::DecodeFailed);
            tracing::warn!(
                file = %path.display(),
                source_format,
                decoder = RAW_DECODER,
                preview_error = %preview_detail,
                error = %mapped,
                open_ms = millis(timings.open),
                preview_ms = millis(timings.preview),
                full_decode_ms = millis(timings.full_decode),
                total_ms = millis(timings.total),
                "RAW decode failed"
            );
            return Err(mapped);
        }
    };

    resize_if_needed(&mut image, max_dim, &mut timings);
    timings.total = total_started.elapsed();
    tracing::debug!(
        file = %path.display(),
        code = %RawErrorCode::PreviewUnavailable,
        "embedded RAW preview unavailable or too small; half-size fallback succeeded"
    );
    log_success(path, &source_format, DecodeMethod::HalfSizeDemosaic, &image, timings);
    Ok(DecodedRaw {
        image,
        source_format,
        method: DecodeMethod::HalfSizeDemosaic,
        timings,
    })
}

fn required_preview_long_edge(max_dim: Option<u32>) -> u32 {
    max_dim.unwrap_or(MIN_PREVIEW_LONG_EDGE).max(MIN_PREVIEW_LONG_EDGE)
}

#[cfg(not(target_os = "macos"))]
fn decode_preview(path: &Path, timings: &mut DecodeTimings) -> std::result::Result<RgbImage, String> {
    let open_started = Instant::now();
    let mut processor = RawProcessor::new().map_err(|error| error.to_string())?;
    processor.open_file(path).map_err(|error| error.to_string())?;
    timings.open += open_started.elapsed();
    let pixels = processor
        .unpack_thumb()
        .and_then(|_| processor.get_thumbnail())
        .map_err(|error| error.to_string())?;
    drop(processor);
    pixels_from_libraw(pixels)
}

#[cfg(target_os = "macos")]
fn decode_preview(path: &Path, timings: &mut DecodeTimings) -> std::result::Result<RgbImage, String> {
    let open_started = Instant::now();
    let image = rawler::analyze::extract_preview_pixels(path, rawler::decoders::RawDecodeParams::default())
        .map_err(|error| error.to_string())?;
    timings.open += open_started.elapsed();
    let rgb = image.into_rgb8();
    RgbImage::from_raw(rgb.width(), rgb.height(), rgb.into_raw())
        .ok_or_else(|| "Rawler preview length does not match its dimensions".to_string())
}

#[cfg(not(target_os = "macos"))]
fn decode_fallback(path: &Path, timings: &mut DecodeTimings) -> std::result::Result<RgbImage, rawlib::RawError> {
    let open_started = Instant::now();
    let mut processor = RawProcessor::new()?;
    processor.open_file(path)?;
    timings.open += open_started.elapsed();
    processor.set_decode_options(&DecodeOptions::preview());
    processor.unpack()?;
    processor.dcraw_process()?;
    let pixels = processor.get_image()?;
    // The processor and its large sensor buffers are dropped before conversion
    // or resizing allocates another image.
    drop(processor);
    pixels_from_libraw(pixels).map_err(|e| rawlib::RawError { code: -1, message: e })
}

#[cfg(target_os = "macos")]
fn decode_fallback(
    path: &Path,
    timings: &mut DecodeTimings,
) -> std::result::Result<RgbImage, rawler::RawlerError> {
    let open_started = Instant::now();
    let image = rawler::analyze::raw_to_srgb(path, rawler::decoders::RawDecodeParams::default())?;
    timings.open += open_started.elapsed();
    let rgb = image.into_rgb8();
    RgbImage::from_raw(rgb.width(), rgb.height(), rgb.into_raw())
        .ok_or_else(|| rawler::RawlerError::DecoderFailed("developed image length does not match its dimensions".into()))
}

#[cfg(not(target_os = "macos"))]
fn pixels_from_libraw(data: ThumbnailData) -> std::result::Result<RgbImage, String> {
    match data.format {
        ImageFormat::Jpeg => image::load_from_memory(&data.data)
            .map(DynamicImage::into_rgb8)
            .map_err(|e| format!("invalid embedded JPEG: {e}")),
        ImageFormat::Bitmap => bitmap_to_rgb(data),
        ImageFormat::Unknown(code) => Err(format!("LibRaw returned unknown image type {code}")),
    }
}

#[cfg(not(target_os = "macos"))]
fn bitmap_to_rgb(mut data: ThumbnailData) -> std::result::Result<RgbImage, String> {
    let width = u32::from(data.width);
    let height = u32::from(data.height);
    let colors = usize::from(data.colors);
    if width == 0 || height == 0 || colors < 3 {
        return Err(format!("invalid bitmap shape {width}x{height}x{colors}"));
    }
    let pixels = usize::try_from(width)
        .ok()
        .and_then(|w| usize::try_from(height).ok().and_then(|h| w.checked_mul(h)))
        .ok_or_else(|| "decoded bitmap dimensions exceed addressable memory".to_string())?;

    let rgb = match data.bits {
        8 if colors == 3 => {
            let expected = pixels
                .checked_mul(3)
                .ok_or_else(|| "decoded bitmap is too large".to_string())?;
            if data.data.len() < expected {
                return Err("decoded bitmap data is truncated".into());
            }
            data.data.truncate(expected);
            data.data
        }
        8 => {
            let expected = pixels
                .checked_mul(colors)
                .ok_or_else(|| "decoded bitmap is too large".to_string())?;
            if data.data.len() < expected {
                return Err("decoded bitmap data is truncated".into());
            }
            data.data
                .chunks_exact(colors)
                .take(pixels)
                .flat_map(|sample| [sample[0], sample[1], sample[2]])
                .collect()
        }
        16 => {
            let expected = pixels
                .checked_mul(colors)
                .and_then(|samples| samples.checked_mul(2))
                .ok_or_else(|| "decoded bitmap is too large".to_string())?;
            if data.data.len() < expected {
                return Err("decoded 16-bit bitmap data is truncated".into());
            }
            data.data
                .chunks_exact(colors * 2)
                .take(pixels)
                .flat_map(|sample| {
                    [
                        (u16::from_ne_bytes([sample[0], sample[1]]) >> 8) as u8,
                        (u16::from_ne_bytes([sample[2], sample[3]]) >> 8) as u8,
                        (u16::from_ne_bytes([sample[4], sample[5]]) >> 8) as u8,
                    ]
                })
                .collect()
        }
        bits => return Err(format!("unsupported LibRaw bitmap depth {bits}")),
    };

    RgbImage::from_raw(width, height, rgb).ok_or_else(|| "decoded bitmap length does not match its dimensions".into())
}

fn resize_if_needed(image: &mut RgbImage, max_dim: Option<u32>, timings: &mut DecodeTimings) {
    let Some(max) = max_dim.filter(|max| image.width().max(image.height()) > *max) else {
        return;
    };
    let started = Instant::now();
    let scaled = DynamicImage::ImageRgb8(std::mem::take(image)).resize(max, max, image::imageops::FilterType::Lanczos3);
    *image = scaled.into_rgb8();
    timings.resize += started.elapsed();
}

fn raw_error(path: &Path, error: impl fmt::Display, fallback: RawErrorCode) -> MediaError {
    let detail = error.to_string();
    let message = detail.to_ascii_lowercase();
    let code = if message.contains("memory")
        || message.contains("allocation")
        || message.contains("too big")
        || message.contains("too large")
    {
        RawErrorCode::OutOfMemory
    } else if message.contains("unsupported") || message.contains("unknown file") {
        RawErrorCode::Unsupported
    } else if message.contains("corrupt")
        || message.contains("data error")
        || message.contains("i/o error")
        || message.contains("input/output error")
    {
        RawErrorCode::Corrupt
    } else {
        fallback
    };
    MediaError::Raw {
        code,
        detail: format!("{}: {detail}", path.display()),
    }
}

fn log_success(path: &Path, source_format: &str, method: DecodeMethod, image: &RgbImage, timings: DecodeTimings) {
    tracing::info!(
        file = %path.display(),
        source_format,
        decoder = RAW_DECODER,
        decode_method = method.as_str(),
        width = image.width(),
        height = image.height(),
        open_ms = millis(timings.open),
        preview_ms = millis(timings.preview),
        full_decode_ms = millis(timings.full_decode),
        resize_ms = millis(timings.resize),
        total_ms = millis(timings.total),
        result = "ok",
        "RAW decoded"
    );
}

fn millis(duration: Duration) -> u128 {
    duration.as_millis()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_codes_are_stable_for_logs_and_ui() {
        assert_eq!(RawErrorCode::Unsupported.to_string(), "RAW_UNSUPPORTED");
        assert_eq!(RawErrorCode::PreviewUnavailable.to_string(), "RAW_PREVIEW_UNAVAILABLE");
        assert_eq!(RawErrorCode::OutOfMemory.to_string(), "RAW_OUT_OF_MEMORY");
    }

    #[test]
    #[cfg(not(target_os = "macos"))]
    fn converts_an_eight_bit_rgb_bitmap() {
        let data = ThumbnailData {
            format: ImageFormat::Bitmap,
            width: 2,
            height: 1,
            colors: 3,
            bits: 8,
            data: vec![255, 0, 0, 0, 0, 255],
        };
        let image = pixels_from_libraw(data).unwrap();
        assert_eq!(image.dimensions(), (2, 1));
        assert_eq!(image.get_pixel(1, 0).0, [0, 0, 255]);
    }

    #[test]
    fn raw_preview_must_meet_the_requested_working_resolution() {
        assert_eq!(required_preview_long_edge(Some(512)), MIN_PREVIEW_LONG_EDGE);
        assert_eq!(required_preview_long_edge(Some(1600)), 1600);
        assert_eq!(required_preview_long_edge(Some(2048)), 2048);
        assert_eq!(required_preview_long_edge(None), MIN_PREVIEW_LONG_EDGE);
    }
}
