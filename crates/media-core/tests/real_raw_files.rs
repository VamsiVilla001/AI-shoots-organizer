//! Exercises the raw preview reader against a folder of real camera files.
//!
//! Ignored by default: raw files are gigabytes and nobody commits them, so a
//! clean checkout has nothing to run against. Point it at a shoot and run it
//! deliberately:
//!
//! ```text
//! TEO_RAW_DIR="//NAS/For Editors/Sort Test/Day 10" \
//!   cargo test -p teo-media-core --test real_raw_files -- --ignored --nocapture
//! ```
//!
//! Unit tests cover the parsing with files this repository builds itself, which
//! proves the arithmetic and nothing about the cameras. Only a real shoot shows
//! whether a preview is where the format says it is, whether it is large enough
//! to be worth using, and whether the EXIF beside it carries the orientation
//! that decides which way up a face ends up.

use std::path::{Path, PathBuf};

use teo_media_core::{decode, metadata, raw};

fn sample_dir() -> Option<PathBuf> {
    let dir = std::env::var("TEO_RAW_DIR").ok()?;
    let path = PathBuf::from(dir);
    path.is_dir().then_some(path)
}

fn raw_files(dir: &Path, limit: usize) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = std::fs::read_dir(dir)
        .expect("the sample directory should be readable")
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| raw::is_raw(path))
        .collect();
    found.sort();
    found.truncate(limit);
    found
}

#[test]
#[ignore = "needs TEO_RAW_DIR pointing at real camera files"]
fn every_raw_file_yields_a_usable_preview() {
    let Some(dir) = sample_dir() else {
        eprintln!("TEO_RAW_DIR is not set to a directory; nothing to do");
        return;
    };

    let files = raw_files(&dir, 500);
    assert!(!files.is_empty(), "no raw files in {}", dir.display());

    let mut usable = 0;
    let mut missing = Vec::new();
    for path in &files {
        match raw::best_preview(path) {
            Some(preview) if preview.is_useful() => {
                usable += 1;
                if usable <= 3 {
                    println!(
                        "  {} -> {}x{} ({} KB)",
                        path.file_name().unwrap().to_string_lossy(),
                        preview.width,
                        preview.height,
                        preview.bytes.len() / 1024
                    );
                }
            }
            Some(preview) => missing.push(format!(
                "{}: only {}x{}",
                path.file_name().unwrap().to_string_lossy(),
                preview.width,
                preview.height
            )),
            None => missing.push(format!("{}: no preview", path.file_name().unwrap().to_string_lossy())),
        }
    }

    println!("{usable} of {} files carry a usable preview", files.len());
    assert!(missing.is_empty(), "files without a usable preview: {missing:?}");
}

#[test]
#[ignore = "needs TEO_RAW_DIR pointing at real camera files"]
fn the_preview_decodes_and_carries_its_orientation() {
    let Some(dir) = sample_dir() else {
        eprintln!("TEO_RAW_DIR is not set to a directory; nothing to do");
        return;
    };

    // Ten is enough to see whether the format was read correctly; decoding
    // hundreds of full-size JPEGs is the pipeline's job, not a test's.
    for path in raw_files(&dir, 10) {
        let meta = metadata::read(
            &path,
            teo_media_core::MediaKind::Photo,
            teo_media_core::Decoder::Ffmpeg,
            None,
        );

        // Dimensions used to come back null for every RAF, because the EXIF
        // reader cannot open the container at all.
        assert!(
            meta.width.is_some() && meta.height.is_some(),
            "{}: no dimensions",
            path.display()
        );
        assert!(
            (1..=8).contains(&meta.orientation),
            "{}: bad orientation",
            path.display()
        );

        // No FFmpeg is passed, so this only succeeds via the embedded preview —
        // which is the whole point for a format FFmpeg cannot open.
        let image = decode::load_image(&path, meta.orientation, Some(1024), None)
            .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        assert!(image.width().max(image.height()) <= 1024);
        assert!(image.width() > 0 && image.height() > 0);

        println!(
            "  {} -> decoded {}x{}, orientation {}, captured {:?}",
            path.file_name().unwrap().to_string_lossy(),
            image.width(),
            image.height(),
            meta.orientation,
            meta.captured_at
        );
    }
}
