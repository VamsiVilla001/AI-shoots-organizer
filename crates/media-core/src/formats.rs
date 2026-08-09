//! Which files the scanner picks up, and how each one has to be decoded.

use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaKind {
    Photo,
    Video,
}

impl MediaKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            MediaKind::Photo => "photo",
            MediaKind::Video => "video",
        }
    }
}

/// How a still image gets turned into pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decoder {
    /// Handled natively by the `image` crate — no external process.
    Native,
    /// Needs FFmpeg. HEIC and camera raw formats have no pure-Rust decoder we
    /// want to depend on, and FFmpeg is already a requirement for video.
    Ffmpeg,
}

/// Formats listed as "initial support" in the architecture plan (§3.2).
const PHOTO_NATIVE: &[&str] = &["jpg", "jpeg", "png", "webp", "tif", "tiff", "bmp"];

/// Formats the plan lists for later support, decoded through FFmpeg.
const PHOTO_FFMPEG: &[&str] = &[
    "heic", "heif", "avif", // Apple / modern still formats
    "cr2", "cr3", "nef", "arw", "dng", "raf", "orf", "rw2", // camera raw
];

const VIDEO: &[&str] = &["mp4", "mov", "mkv", "avi", "webm", "m4v", "mpg", "mpeg", "wmv", "mts", "m2ts"];

/// Returns `None` for anything that is not media we can index.
pub fn classify(path: &Path) -> Option<(MediaKind, Decoder)> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    if PHOTO_NATIVE.contains(&ext.as_str()) {
        Some((MediaKind::Photo, Decoder::Native))
    } else if PHOTO_FFMPEG.contains(&ext.as_str()) {
        Some((MediaKind::Photo, Decoder::Ffmpeg))
    } else if VIDEO.contains(&ext.as_str()) {
        Some((MediaKind::Video, Decoder::Ffmpeg))
    } else {
        None
    }
}

pub fn is_supported(path: &Path) -> bool {
    classify(path).is_some()
}

pub fn extension(path: &Path) -> String {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default()
}

/// Every extension the scanner recognises, for showing in the UI.
pub fn supported_extensions() -> Vec<&'static str> {
    PHOTO_NATIVE
        .iter()
        .chain(PHOTO_FFMPEG.iter())
        .chain(VIDEO.iter())
        .copied()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_by_extension_case_insensitively() {
        assert_eq!(classify(Path::new("a/IMG_0231.JPG")), Some((MediaKind::Photo, Decoder::Native)));
        assert_eq!(classify(Path::new("a/clip.MP4")), Some((MediaKind::Video, Decoder::Ffmpeg)));
        assert_eq!(classify(Path::new("a/shot.heic")), Some((MediaKind::Photo, Decoder::Ffmpeg)));
        assert_eq!(classify(Path::new("a/raw.CR3")), Some((MediaKind::Photo, Decoder::Ffmpeg)));
    }

    #[test]
    fn ignores_non_media() {
        assert!(classify(Path::new("notes.txt")).is_none());
        assert!(classify(Path::new("Thumbs.db")).is_none());
        assert!(classify(Path::new("no_extension")).is_none());
    }
}
