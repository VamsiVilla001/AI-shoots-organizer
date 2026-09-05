//! Media scanning, metadata, decoding and thumbnails for the Esports AI Media
//! Organiser.
//!
//! This crate is the boundary between the user's files and everything else.
//! It only ever *reads* from the shoot folder; every byte it writes goes to the
//! application's own data directory.

pub mod decode;
pub mod ffmpeg;
pub mod formats;
pub mod gstreamer;
pub mod metadata;
pub mod proxies;
pub mod quality;
pub mod raw;
pub mod scanner;
pub mod thumbnails;

pub use ffmpeg::Ffmpeg;
pub use formats::{Decoder, MediaKind};
pub use gstreamer::Gstreamer;
pub use metadata::{Metadata, Orientation};
pub use proxies::{VideoProxyCache, VIDEO_PROXY_WIDTH};
pub use scanner::{scan, ScanOptions, ScanReport, ScannedFile};
pub use thumbnails::{ThumbnailCache, THUMBNAIL_MAX_DIM};

#[derive(Debug, thiserror::Error)]
pub enum MediaError {
    #[error("file or folder not found: {0}")]
    NotFound(String),
    #[error("unsupported media: {0}")]
    Unsupported(String),
    #[error("could not decode: {0}")]
    Decode(String),
    #[error("{code}: {detail}")]
    Raw { code: raw::RawErrorCode, detail: String },
    #[error("could not encode: {0}")]
    Encode(String),
    #[error("FFmpeg is required but not available: {0}")]
    MissingFfmpeg(String),
    #[error("FFmpeg error: {0}")]
    Ffmpeg(String),
    #[error("GStreamer is required but not available: {0}")]
    MissingGstreamer(String),
    #[error("GStreamer error: {0}")]
    Gstreamer(String),
    #[error("io error: {0}")]
    Io(String),
}

pub type Result<T> = std::result::Result<T, MediaError>;

impl From<std::io::Error> for MediaError {
    fn from(e: std::io::Error) -> Self {
        MediaError::Io(e.to_string())
    }
}
