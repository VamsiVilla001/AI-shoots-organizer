//! EXIF and container metadata (§3.2).

use std::fs::File;
use std::io::BufReader;
use std::path::Path;

use exif::{In, Tag, Value};

use crate::ffmpeg::Ffmpeg;
use crate::formats::{Decoder, MediaKind};

#[derive(Debug, Clone, PartialEq)]
pub struct Metadata {
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub duration: Option<f64>,
    /// RFC 3339, from EXIF `DateTimeOriginal` where present.
    pub captured_at: Option<String>,
    pub camera_make: Option<String>,
    pub camera_model: Option<String>,
    pub lens: Option<String>,
    pub iso: Option<u32>,
    pub focal_length: Option<f64>,
    pub aperture: Option<f64>,
    pub shutter: Option<String>,
    /// EXIF orientation, 1–8. 1 means "already upright".
    pub orientation: u16,
}

impl Default for Metadata {
    fn default() -> Self {
        Self {
            width: None,
            height: None,
            duration: None,
            captured_at: None,
            camera_make: None,
            camera_model: None,
            lens: None,
            iso: None,
            focal_length: None,
            aperture: None,
            shutter: None,
            // 1 is "upright"; 0 is not a valid EXIF orientation.
            orientation: 1,
        }
    }
}

/// The eight EXIF orientations, as the transform needed to make the image
/// upright.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Orientation {
    #[default]
    Normal,
    FlipHorizontal,
    Rotate180,
    FlipVertical,
    Transpose,
    Rotate90,
    Transverse,
    Rotate270,
}

impl Orientation {
    pub fn from_exif(value: u16) -> Self {
        match value {
            2 => Orientation::FlipHorizontal,
            3 => Orientation::Rotate180,
            4 => Orientation::FlipVertical,
            5 => Orientation::Transpose,
            6 => Orientation::Rotate90,
            7 => Orientation::Transverse,
            8 => Orientation::Rotate270,
            _ => Orientation::Normal,
        }
    }

    /// Whether applying this orientation swaps width and height.
    pub fn swaps_axes(&self) -> bool {
        matches!(
            self,
            Orientation::Transpose | Orientation::Rotate90 | Orientation::Transverse | Orientation::Rotate270
        )
    }
}

/// Reads whatever metadata the file offers. Never fails: a photo with no EXIF
/// at all still needs to be indexed, so missing fields come back as `None`.
pub fn read(path: &Path, kind: MediaKind, decoder: Decoder, ffmpeg: Option<&Ffmpeg>) -> Metadata {
    let mut meta = Metadata::default();

    match kind {
        MediaKind::Photo => {
            read_exif_into(path, &mut meta);

            if decoder == Decoder::Native {
                if let Ok((w, h)) = image::image_dimensions(path) {
                    meta.width = Some(w);
                    meta.height = Some(h);
                }
            } else if let Some(ff) = ffmpeg {
                // HEIC and raw files still carry EXIF, but their dimensions
                // have to come from the decoder.
                if let Ok(info) = ff.probe(path) {
                    meta.width = meta.width.or(info.width);
                    meta.height = meta.height.or(info.height);
                }
            }
        }
        MediaKind::Video => {
            if let Some(ff) = ffmpeg {
                if let Ok(info) = ff.probe(path) {
                    meta.width = info.width;
                    meta.height = info.height;
                    meta.duration = info.duration;
                    meta.captured_at = info.creation_time.and_then(|t| normalise_timestamp(&t));
                    // Container rotation maps onto the same orientation codes
                    // the rest of the pipeline already understands.
                    meta.orientation = match info.rotation.rem_euclid(360) {
                        90 => 6,
                        180 => 3,
                        270 => 8,
                        _ => 1,
                    };
                }
            }
        }
    }

    if meta.captured_at.is_none() {
        meta.captured_at = file_modified_at(path);
    }
    meta
}

fn read_exif_into(path: &Path, meta: &mut Metadata) {
    let Ok(file) = File::open(path) else { return };
    let mut reader = BufReader::new(file);
    let Ok(exif) = exif::Reader::new().read_from_container(&mut reader) else {
        return; // PNG, WEBP and stripped JPEGs simply have nothing to read.
    };

    if let Some(field) = exif.get_field(Tag::Orientation, In::PRIMARY) {
        if let Some(v) = field.value.get_uint(0) {
            meta.orientation = v as u16;
        }
    }

    for tag in [Tag::DateTimeOriginal, Tag::DateTimeDigitized, Tag::DateTime] {
        if meta.captured_at.is_some() {
            break;
        }
        if let Some(field) = exif.get_field(tag, In::PRIMARY) {
            if let Value::Ascii(ref values) = field.value {
                if let Some(first) = values.first() {
                    if let Ok(dt) = exif::DateTime::from_ascii(first) {
                        meta.captured_at = Some(format!(
                            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
                            dt.year, dt.month, dt.day, dt.hour, dt.minute, dt.second
                        ));
                    }
                }
            }
        }
    }

    meta.camera_make = ascii_field(&exif, Tag::Make);
    meta.camera_model = ascii_field(&exif, Tag::Model);
    meta.lens = ascii_field(&exif, Tag::LensModel);
    meta.iso = exif
        .get_field(Tag::PhotographicSensitivity, In::PRIMARY)
        .and_then(|f| f.value.get_uint(0));
    meta.focal_length = rational_field(&exif, Tag::FocalLength);
    meta.aperture = rational_field(&exif, Tag::FNumber);
    meta.shutter = exif
        .get_field(Tag::ExposureTime, In::PRIMARY)
        .map(|f| f.display_value().to_string());

    // EXIF dimensions are a useful fallback but the decoder is authoritative,
    // so only fill in what is still missing.
    if meta.width.is_none() {
        meta.width = exif
            .get_field(Tag::PixelXDimension, In::PRIMARY)
            .and_then(|f| f.value.get_uint(0));
        meta.height = exif
            .get_field(Tag::PixelYDimension, In::PRIMARY)
            .and_then(|f| f.value.get_uint(0));
    }
}

fn ascii_field(exif: &exif::Exif, tag: Tag) -> Option<String> {
    let field = exif.get_field(tag, In::PRIMARY)?;
    let text = field.display_value().to_string();
    let text = text.trim().trim_matches('"').trim().to_string();
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

fn rational_field(exif: &exif::Exif, tag: Tag) -> Option<f64> {
    let field = exif.get_field(tag, In::PRIMARY)?;
    match field.value {
        Value::Rational(ref v) => v.first().map(|r| r.to_f64()),
        Value::SRational(ref v) => v.first().map(|r| r.to_f64()),
        _ => None,
    }
}

/// Filesystem modification time, used when the file carries no capture date.
pub fn file_modified_at(path: &Path) -> Option<String> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    let datetime: chrono::DateTime<chrono::Utc> = modified.into();
    Some(datetime.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
}

/// FFmpeg reports `creation_time` in a few shapes; normalise to RFC 3339.
fn normalise_timestamp(raw: &str) -> Option<String> {
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(raw) {
        return Some(dt.to_rfc3339_opts(chrono::SecondsFormat::Secs, true));
    }
    if let Ok(naive) = chrono::NaiveDateTime::parse_from_str(raw, "%Y-%m-%d %H:%M:%S") {
        return Some(naive.and_utc().to_rfc3339_opts(chrono::SecondsFormat::Secs, true));
    }
    if let Ok(naive) = chrono::NaiveDateTime::parse_from_str(raw, "%Y-%m-%dT%H:%M:%S%.f") {
        return Some(naive.and_utc().to_rfc3339_opts(chrono::SecondsFormat::Secs, true));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orientation_codes_map_to_transforms() {
        assert_eq!(Orientation::from_exif(1), Orientation::Normal);
        assert_eq!(Orientation::from_exif(6), Orientation::Rotate90);
        assert_eq!(Orientation::from_exif(8), Orientation::Rotate270);
        // Out-of-range values must degrade to "leave it alone", not panic.
        assert_eq!(Orientation::from_exif(99), Orientation::Normal);

        assert!(Orientation::Rotate90.swaps_axes());
        assert!(!Orientation::Rotate180.swaps_axes());
    }

    #[test]
    fn normalises_ffmpeg_timestamps() {
        assert_eq!(
            normalise_timestamp("2026-08-09T10:45:21.000000Z").as_deref(),
            Some("2026-08-09T10:45:21Z")
        );
        assert_eq!(
            normalise_timestamp("2026-08-09 10:45:21").as_deref(),
            Some("2026-08-09T10:45:21Z")
        );
        assert_eq!(normalise_timestamp("nonsense"), None);
    }
}
