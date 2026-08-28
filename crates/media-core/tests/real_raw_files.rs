//! Opt-in tests against real camera files. Set `TEO_RAW_FILE` to one RAW path.

use std::path::{Path, PathBuf};

use teo_media_core::decode::{self, DecodeMethod};
use teo_media_core::formats::{self, Decoder, MediaKind};

fn sample() -> Option<PathBuf> {
    std::env::var_os("TEO_RAW_FILE")
        .map(PathBuf::from)
        .filter(|path| path.is_file())
}

#[test]
#[ignore = "set TEO_RAW_FILE to a real camera RAW"]
fn raw_is_verified_and_decoded_without_ffmpeg() {
    let Some(path) = sample() else {
        eprintln!("TEO_RAW_FILE is not set to a readable file");
        return;
    };
    assert_eq!(formats::classify(&path), Some((MediaKind::Photo, Decoder::LibRaw)));

    let metadata = teo_media_core::metadata::read(&path, MediaKind::Photo, Decoder::LibRaw, None);
    let decoded = decode::decode_image(&path, metadata.orientation, Some(1600), None)
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()));

    assert!(decoded.image.width() > 0 && decoded.image.height() > 0);
    assert!(decoded.image.width().max(decoded.image.height()) <= 1600);
    assert!(matches!(
        decoded.decode_method,
        DecodeMethod::LibRawEmbeddedPreview | DecodeMethod::LibRawHalfSizeDemosaic
    ));
    let quality = teo_media_core::quality::analyse(&decoded.image);
    assert!((0.0..=1.0).contains(&quality.overall));
    assert!((0.0..=1.0).contains(&quality.sharpness));
    assert!((0.0..=1.0).contains(&quality.exposure));

    let cache_dir = tempfile::tempdir().unwrap();
    let cache = teo_media_core::ThumbnailCache::new(cache_dir.path());
    let first = cache
        .ensure(&path, "real-raw-cache-key", metadata.orientation, None, None)
        .expect("RAW thumbnail generation must not need FFmpeg");
    let second = cache
        .ensure(&path, "real-raw-cache-key", metadata.orientation, None, None)
        .expect("the existing content-key cache should be reusable");
    assert_eq!(first, second);
    assert!(first.is_file());

    println!(
        "{} -> {}x{}, {}, orientation {}",
        path.display(),
        decoded.image.width(),
        decoded.image.height(),
        decoded.decode_method.as_str(),
        metadata.orientation
    );
}

#[test]
fn acceptance_formats_route_to_the_expected_pipeline() {
    for extension in [
        "RAF", "ARW", "NEF", "CR2", "CR3", "DNG", "ORF", "RW2", "PEF", "SRW", "3FR", "IIQ", "RWL", "RAW",
    ] {
        assert_eq!(
            formats::classify(Path::new(&format!("photo.{extension}"))),
            Some((MediaKind::Photo, Decoder::LibRaw)),
            "{extension} must never be sent to FFmpeg"
        );
    }
    for extension in ["JPG", "JPEG", "PNG"] {
        assert_eq!(
            formats::classify(Path::new(&format!("photo.{extension}"))),
            Some((MediaKind::Photo, Decoder::Native))
        );
    }
    for extension in ["MP4", "MOV"] {
        assert_eq!(
            formats::classify(Path::new(&format!("video.{extension}"))),
            Some((MediaKind::Video, Decoder::Ffmpeg))
        );
    }
}
