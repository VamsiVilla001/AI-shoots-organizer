//! Per-file analysis: decode, detect, embed, persist.
//!
//! An [`Engine`] owns one detector session and one embedder session and is
//! therefore **not** shared between threads — each worker builds its own. That
//! is deliberate: ONNX Runtime sessions need exclusive access to run, and
//! loading the models once per worker rather than once per file is the "avoid
//! loading models repeatedly" rule from §19.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Instant;

use image::RgbImage;
use teo_database::models::{BoundingBox, Media, MediaMetadata, NewFace, ProcessingStatus};
use teo_database::repo::{faces, media as media_repo, video as video_repo};
use teo_database::Database;
use teo_face_detection::{Detection, FaceDetector, Rect, ScrfdDetector};
use teo_face_recognition::{ArcFaceEmbedder, FaceEmbedder};
use teo_media_core::formats::{self, MediaKind};
use teo_media_core::{Ffmpeg, ThumbnailCache};

use crate::models::{ModelRegistry, ModelRole};
use crate::paths::AppPaths;
use crate::settings::AppSettings;

#[derive(Debug, thiserror::Error)]
pub enum PipelineError {
    #[error("face models are not installed: {0}")]
    ModelsUnavailable(String),
    #[error("FFmpeg is required for this file but is not available")]
    FfmpegUnavailable,
    #[error(transparent)]
    Media(#[from] teo_media_core::MediaError),
    #[error(transparent)]
    Database(#[from] teo_database::DbError),
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, PipelineError>;

/// What one file's analysis produced.
#[derive(Debug, Clone, Default)]
pub struct AnalysisOutcome {
    pub faces_detected: usize,
    pub faces_embedded: usize,
    /// Distinct frames looked at. Always 1 for a photo.
    pub frames_analysed: usize,
}

pub struct Engine {
    detector: ScrfdDetector,
    embedder: ArcFaceEmbedder,
    ffmpeg: Option<Ffmpeg>,
    settings: AppSettings,
}

/// ONNX Runtime's DirectML provider can corrupt the native heap when several
/// large sessions are created concurrently. Workers still own and run their
/// engines independently, but construction must pass through this process-wide
/// gate so provider and model initialisation happen one engine at a time.
fn with_serialized_engine_initialization<T>(build: impl FnOnce() -> T) -> T {
    static ENGINE_INITIALIZATION: OnceLock<parking_lot::Mutex<()>> = OnceLock::new();
    let _guard = ENGINE_INITIALIZATION.get_or_init(|| parking_lot::Mutex::new(())).lock();
    build()
}

impl Engine {
    /// Builds an engine, loading both models. Fails when either is missing —
    /// the caller turns that into a message pointing at the model setup step
    /// rather than a stack trace.
    pub fn new(paths: &AppPaths, settings: &AppSettings) -> Result<Self> {
        with_serialized_engine_initialization(|| Self::new_unlocked(paths, settings))
    }

    fn new_unlocked(paths: &AppPaths, settings: &AppSettings) -> Result<Self> {
        let registry = ModelRegistry::new(&paths.models);
        let status = registry.status(settings.detector_model.as_deref(), settings.embedder_model.as_deref());
        if !status.ready {
            return Err(PipelineError::ModelsUnavailable(status.message));
        }

        let detector_path = registry
            .resolve(ModelRole::Detector, settings.detector_model.as_deref())
            .ok_or_else(|| PipelineError::ModelsUnavailable(status.message.clone()))?;
        let embedder_path = registry
            .resolve(ModelRole::Embedder, settings.embedder_model.as_deref())
            .ok_or_else(|| PipelineError::ModelsUnavailable(status.message.clone()))?;

        let session_config = settings.session_config();
        let detector = ScrfdDetector::load(&detector_path, settings.detector_config(), &session_config)
            .map_err(|e| PipelineError::Other(format!("loading detector: {e}")))?;
        let embedder = ArcFaceEmbedder::load(&embedder_path, &session_config)
            .map_err(|e| PipelineError::Other(format!("loading embedder: {e}")))?;

        Ok(Self {
            detector,
            embedder,
            ffmpeg: discover_ffmpeg(settings),
            settings: settings.clone(),
        })
    }

    pub fn detector_name(&self) -> &str {
        self.detector.name()
    }

    pub fn embedder_name(&self) -> &str {
        self.embedder.name()
    }

    pub fn ffmpeg(&self) -> Option<&Ffmpeg> {
        self.ffmpeg.as_ref()
    }

    /// Extracts a recognition vector from a box a reviewer drew around a
    /// missed face. Manual boxes have no five-point landmarks, so the
    /// embedder deliberately uses its padded bounding-box alignment fallback.
    /// The returned box remains normalised against the oriented image, exactly
    /// like detector-produced boxes.
    pub fn embed_manual_face(&mut self, item: &Media, bbox: BoundingBox) -> Result<(Vec<f32>, f64)> {
        let path = Path::new(&item.path);
        let orientation = source_media_orientation(item, MediaKind::Photo, self.ffmpeg.as_ref());
        let image = teo_media_core::decode::load_image(
            path,
            orientation,
            Some(self.settings.analysis_max_dim),
            self.ffmpeg.as_ref(),
        )?;
        let (width, height) = image.dimensions();
        let rect = Rect {
            x1: (bbox.x * width as f64) as f32,
            y1: (bbox.y * height as f64) as f32,
            x2: ((bbox.x + bbox.w) * width as f64) as f32,
            y2: ((bbox.y + bbox.h) * height as f64) as f32,
        };
        if rect.width() < 12.0 || rect.height() < 12.0 {
            return Err(PipelineError::Other("draw a slightly larger box around the face".into()));
        }

        let detection = Detection { bbox: rect, score: 1.0, landmarks: None };
        let quality = detection.quality(width, height);
        let embedding = self
            .embedder
            .embed(&image, &detection)
            .map_err(|e| PipelineError::Other(format!("could not read the manually marked face: {e}")))?;
        Ok((embedding.into_vec(), quality))
    }

    /// Full analysis of a still image.
    pub fn analyse_photo(&mut self, db: &Database, item: &Media) -> Result<AnalysisOutcome> {
        let analysis_started = Instant::now();
        let path = PathBuf::from(&item.path);
        // Indexing is the normal source of metadata, but reading EXIF again is
        // cheap compared with inference and protects this coordinate system
        // even if a stale database row came from an older build.
        let orientation = source_media_orientation(item, MediaKind::Photo, self.ffmpeg.as_ref());
        if i64::from(orientation) != item.orientation {
            tracing::warn!(
                media = item.id,
                indexed = item.orientation,
                source = orientation,
                "correcting stale photo orientation before face analysis"
            );
            let conn = db.conn()?;
            media_repo::set_orientation(&conn, item.id, i64::from(orientation))?;
        }

        let decoded = teo_media_core::decode::decode_image(
            &path,
            orientation,
            Some(self.settings.analysis_max_dim),
            self.ffmpeg.as_ref(),
        )?;

        // Re-analysis must replace, not append.
        {
            let conn = db.conn()?;
            faces::delete_for_media(&conn, item.id)?;
        }

        let ai_started = Instant::now();
        let outcome = self.detect_and_store(db, item, &decoded.image, None)?;
        let ai_elapsed = ai_started.elapsed();

        {
            let conn = db.conn()?;
            media_repo::set_status(&conn, item.id, ProcessingStatus::Analysed, None)?;
            media_repo::refresh_face_count(&conn, item.id)?;
        }
        tracing::info!(
            file = %item.filename,
            source_format = %decoded.source_format,
            decode_method = decoded.decode_method.as_str(),
            open_ms = decoded.timings.open.as_millis(),
            preview_ms = decoded.timings.preview.as_millis(),
            full_decode_ms = decoded.timings.full_decode.as_millis(),
            resize_ms = decoded.timings.resize.as_millis(),
            ai_ms = ai_elapsed.as_millis(),
            total_ms = analysis_started.elapsed().as_millis(),
            faces = outcome.faces_detected,
            result = "ok",
            "photo analysis complete"
        );
        Ok(outcome)
    }

    /// Full analysis of a video: sample frames, then treat each like a photo.
    pub fn analyse_video(&mut self, db: &Database, item: &Media) -> Result<AnalysisOutcome> {
        let Some(ffmpeg) = self.ffmpeg.clone() else {
            return Err(PipelineError::FfmpegUnavailable);
        };
        let path = PathBuf::from(&item.path);
        let orientation = source_media_orientation(item, MediaKind::Video, Some(&ffmpeg));
        if i64::from(orientation) != item.orientation {
            tracing::warn!(
                media = item.id,
                indexed = item.orientation,
                source = orientation,
                "correcting stale video orientation before face analysis"
            );
            let conn = db.conn()?;
            media_repo::set_orientation(&conn, item.id, i64::from(orientation))?;
        }
        let config = self.settings.video_config();

        let plan = teo_video_analysis::plan_video(&ffmpeg, &path, item.duration, &config);
        let frames = teo_video_analysis::sample_frames(&ffmpeg, &path, &plan, orientation, &config);

        {
            let conn = db.conn()?;
            faces::delete_for_media(&conn, item.id)?;
            video_repo::delete_for_media(&conn, item.id)?;
        }

        let mut outcome = AnalysisOutcome::default();
        for frame in &frames {
            let frame_outcome = self.detect_and_store(db, item, &frame.image, Some(frame.timestamp))?;
            outcome.faces_detected += frame_outcome.faces_detected;
            outcome.faces_embedded += frame_outcome.faces_embedded;
            outcome.frames_analysed += 1;
        }

        {
            let conn = db.conn()?;
            media_repo::set_status(&conn, item.id, ProcessingStatus::Analysed, None)?;
            media_repo::refresh_face_count(&conn, item.id)?;
        }

        tracing::debug!(
            video = %item.filename,
            planned = plan.len(),
            decoded = frames.len(),
            faces = outcome.faces_detected,
            "video analysed"
        );
        Ok(outcome)
    }

    /// Detects faces in one frame, embeds them in a single batch, and writes
    /// the rows. `frame_time` is `None` for stills.
    fn detect_and_store(
        &mut self,
        db: &Database,
        item: &Media,
        image: &RgbImage,
        frame_time: Option<f64>,
    ) -> Result<AnalysisOutcome> {
        let detections: Vec<Detection> = self
            .detector
            .detect(image)
            .map_err(|e| PipelineError::Other(format!("detection failed: {e}")))?;

        if detections.is_empty() {
            return Ok(AnalysisOutcome { frames_analysed: 1, ..Default::default() });
        }

        // One inference call for every face in the frame (§19).
        let embeddings = self.embedder.embed_batch(image, &detections);

        let (width, height) = image.dimensions();
        let mut outcome = AnalysisOutcome { frames_analysed: 1, ..Default::default() };

        let conn = db.conn()?;
        for (detection, embedding) in detections.iter().zip(embeddings) {
            let (x, y, w, h) = detection.bbox.normalised(width, height);
            let embedding = match embedding {
                Ok(e) => Some(e.into_vec()),
                Err(e) => {
                    // Keep the detection so the box still shows in review, but
                    // without a vector it cannot be matched or clustered.
                    tracing::debug!(file = %item.filename, error = %e, "embedding failed for one face");
                    None
                }
            };
            if embedding.is_some() {
                outcome.faces_embedded += 1;
            }

            let face_id = faces::insert(
                &conn,
                &NewFace {
                    media_id: item.id,
                    shoot_id: item.shoot_id,
                    bbox: BoundingBox { x, y, w, h },
                    landmarks: detection.landmarks.map(|lm| {
                        lm.iter().flat_map(|(px, py)| [*px, *py]).collect::<Vec<f32>>()
                    }),
                    detection_confidence: detection.score as f64,
                    embedding,
                    quality: Some(detection.quality(width, height)),
                    frame_time,
                    crop_path: None,
                },
            )?;
            outcome.faces_detected += 1;

            // Videos additionally get a timeline entry, filled in with a person
            // once recognition runs.
            if let Some(at) = frame_time {
                video_repo::insert(&conn, item.id, None, Some(face_id), at, detection.score as f64)?;
            }
        }

        Ok(outcome)
    }

    /// Routes to the photo or video path based on the file's type.
    pub fn analyse(&mut self, db: &Database, item: &Media) -> Result<AnalysisOutcome> {
        match formats::classify(Path::new(&item.path)).map(|(kind, _)| kind) {
            Some(MediaKind::Video) => {
                if !self.settings.video_enabled {
                    let conn = db.conn()?;
                    media_repo::set_status(&conn, item.id, ProcessingStatus::Skipped, Some("video analysis is off"))?;
                    return Ok(AnalysisOutcome::default());
                }
                self.analyse_video(db, item)
            }
            Some(MediaKind::Photo) => self.analyse_photo(db, item),
            None => Err(PipelineError::Other(format!("unsupported file: {}", item.path))),
        }
    }
}

/// Reads orientation from the source immediately before pixels are decoded.
/// A missing/invalid orientation falls back to the indexed value so a transient
/// source-read failure cannot turn a valid rotation into the default upright
/// orientation. The decode path reports missing or unsupported files itself.
fn source_media_orientation(item: &Media, expected_kind: MediaKind, ffmpeg: Option<&Ffmpeg>) -> u16 {
    let indexed = item.orientation.clamp(1, 8) as u16;
    let path = Path::new(&item.path);
    let Some((kind, _)) = formats::classify(path) else {
        return indexed;
    };
    if kind != expected_kind || !path.exists() {
        return indexed;
    }
    teo_media_core::metadata::read_orientation(path, kind, ffmpeg).unwrap_or(indexed)
}

/// Finds FFmpeg, honouring an explicit directory from Settings.
pub fn discover_ffmpeg(settings: &AppSettings) -> Option<Ffmpeg> {
    let hint = settings.ffmpeg_directory.as_ref().map(PathBuf::from);
    Ffmpeg::discover(hint.as_deref())
}

/// Reads metadata and writes a thumbnail. Cheap enough to run over a whole
/// shoot before any AI starts, which is what makes the media grid usable while
/// analysis is still going (§19).
///
/// Deliberately a free function rather than an [`Engine`] method: indexing
/// must never depend on the AI models being installed. A machine without
/// models still gets a fully browsable shoot — only recognition waits.
pub fn index_media(db: &Database, thumbnails: &ThumbnailCache, ffmpeg: Option<&Ffmpeg>, item: &Media) -> Result<()> {
    let path = PathBuf::from(&item.path);
    if !path.exists() {
        let conn = db.conn()?;
        media_repo::set_status(&conn, item.id, ProcessingStatus::Failed, Some("file no longer exists"))?;
        return Err(PipelineError::Other(format!("{} no longer exists", item.path)));
    }

    let (kind, decoder) = formats::classify(&path)
        .ok_or_else(|| PipelineError::Other(format!("unsupported file: {}", item.path)))?;
    let meta = teo_media_core::metadata::read(&path, kind, decoder, ffmpeg);

    {
        let conn = db.conn()?;
        media_repo::set_metadata(
            &conn,
            item.id,
            &MediaMetadata {
                width: meta.width.map(|v| v as i64),
                height: meta.height.map(|v| v as i64),
                duration: meta.duration,
                captured_at: meta.captured_at.clone(),
                camera_make: meta.camera_make.clone(),
                camera_model: meta.camera_model.clone(),
                lens: meta.lens.clone(),
                iso: meta.iso.map(|v| v as i64),
                focal_length: meta.focal_length,
                aperture: meta.aperture,
                shutter: meta.shutter.clone(),
                orientation: meta.orientation as i64,
            },
        )?;
    }

    match thumbnails.ensure(&path, &item.content_key, meta.orientation, meta.duration, ffmpeg) {
        Ok(thumb) => {
            let conn = db.conn()?;
            media_repo::set_thumbnail(&conn, item.id, &thumb.display().to_string())?;
            if kind == MediaKind::Photo {
                match image::open(&thumb) {
                    Ok(image) => {
                        let quality = teo_media_core::quality::analyse(&image.to_rgb8());
                        media_repo::set_quality(
                            &conn,
                            item.id,
                            quality.overall,
                            quality.sharpness,
                            quality.exposure,
                            quality.perceptual_hash,
                        )?;
                    }
                    Err(error) => {
                        tracing::warn!(file = %item.path, %error, "photo quality analysis failed");
                    }
                }
            }
            media_repo::set_status(&conn, item.id, ProcessingStatus::Thumbnailed, None)?;
        }
        Err(e) => {
            // A missing thumbnail is a cosmetic failure; the file can still
            // be analysed and exported, so it must not stop the pipeline.
            tracing::warn!(file = %item.path, error = %e, "thumbnail generation failed");
            let conn = db.conn()?;
            media_repo::set_status(&conn, item.id, ProcessingStatus::Indexed, None)?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Barrier};
    use teo_database::models::{MediaType, NewMedia};
    use teo_database::repo::shoots;

    /// Writes a real JPEG so the decode path is genuinely exercised.
    fn write_jpeg(path: &std::path::Path) {
        let mut image = RgbImage::new(320, 240);
        for (x, y, pixel) in image.enumerate_pixels_mut() {
            *pixel = image::Rgb([(x % 256) as u8, (y % 256) as u8, 128]);
        }
        image.save(path).expect("failed to write the test jpeg");
    }

    /// Adds a minimal EXIF APP1 segment with only the orientation tag. Keeping
    /// this fixture local avoids depending on a camera file or an EXIF writer.
    fn write_oriented_jpeg(path: &std::path::Path, orientation: u16) {
        write_jpeg(path);
        let jpeg = std::fs::read(path).unwrap();
        assert_eq!(&jpeg[..2], &[0xff, 0xd8]);

        let mut exif = vec![
            0xff, 0xe1, 0x00, 0x22, // APP1, 34 bytes including this length
            b'E', b'x', b'i', b'f', 0, 0,
            b'I', b'I', 0x2a, 0x00, 0x08, 0x00, 0x00, 0x00, // little-endian TIFF
            0x01, 0x00, // one IFD entry
            0x12, 0x01, // Orientation tag
            0x03, 0x00, // SHORT
            0x01, 0x00, 0x00, 0x00, // count 1
            orientation as u8, (orientation >> 8) as u8, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, // no next IFD
        ];
        let mut with_exif = Vec::with_capacity(jpeg.len() + exif.len());
        with_exif.extend_from_slice(&jpeg[..2]);
        with_exif.append(&mut exif);
        with_exif.extend_from_slice(&jpeg[2..]);
        std::fs::write(path, with_exif).unwrap();
    }

    #[test]
    fn engine_initialization_is_process_wide_serialized() {
        const THREADS: usize = 8;
        let ready = Arc::new(Barrier::new(THREADS));
        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));

        let handles: Vec<_> = (0..THREADS)
            .map(|_| {
                let ready = Arc::clone(&ready);
                let active = Arc::clone(&active);
                let peak = Arc::clone(&peak);
                std::thread::spawn(move || {
                    ready.wait();
                    with_serialized_engine_initialization(|| {
                        let now = active.fetch_add(1, Ordering::SeqCst) + 1;
                        peak.fetch_max(now, Ordering::SeqCst);
                        std::thread::sleep(std::time::Duration::from_millis(10));
                        active.fetch_sub(1, Ordering::SeqCst);
                    });
                })
            })
            .collect();

        for handle in handles {
            handle.join().unwrap();
        }

        assert_eq!(peak.load(Ordering::SeqCst), 1);
    }

    /// The regression this guards: indexing used to be a method on [`Engine`],
    /// so it demanded both ONNX models. On a machine without them every
    /// thumbnail job failed instantly, burned its retries and buried the UI in
    /// identical errors — while the shoot could have been perfectly browsable.
    #[test]
    fn indexing_works_with_no_models_and_no_ffmpeg() {
        let source = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let photo = source.path().join("IMG_0231.jpg");
        write_jpeg(&photo);

        let db = Database::open_in_memory().unwrap();
        let item = {
            let conn = db.conn().unwrap();
            let shoot = shoots::create(&conn, "Shoot", &source.path().display().to_string()).unwrap();
            let media_id = media_repo::upsert(
                &conn,
                &NewMedia {
                    shoot_id: shoot.id,
                    path: photo.display().to_string(),
                    filename: "IMG_0231.jpg".into(),
                    media_type: MediaType::Photo,
                    extension: "jpg".into(),
                    file_size: 1,
                    content_key: "testkey".into(),
                    captured_at: None,
                },
            )
            .unwrap();
            media_repo::get_by_id(&conn, media_id).unwrap().unwrap()
        };

        let thumbnails = ThumbnailCache::new(cache.path());
        // No models directory, no FFmpeg — both deliberately absent.
        index_media(&db, &thumbnails, None, &item).expect("indexing must not need AI models");

        let conn = db.conn().unwrap();
        let stored = media_repo::get_by_id(&conn, item.id).unwrap().unwrap();
        assert_eq!(stored.processing_status, "thumbnailed");
        assert!(stored.thumbnail_path.is_some(), "a thumbnail should have been produced");
        assert_eq!(stored.width, Some(320));
        assert_eq!(stored.height, Some(240));
        assert!(std::path::Path::new(&stored.thumbnail_path.unwrap()).is_file());
    }

    #[test]
    fn analysis_re_reads_orientation_from_the_source() {
        let source = tempfile::tempdir().unwrap();
        let photo = source.path().join("portrait.jpg");
        write_oriented_jpeg(&photo, 6);

        let db = Database::open_in_memory().unwrap();
        let item = {
            let conn = db.conn().unwrap();
            let shoot = shoots::create(&conn, "Shoot", &source.path().display().to_string()).unwrap();
            let media_id = media_repo::upsert(
                &conn,
                &NewMedia {
                    shoot_id: shoot.id,
                    path: photo.display().to_string(),
                    filename: "portrait.jpg".into(),
                    media_type: MediaType::Photo,
                    extension: "jpg".into(),
                    file_size: 1,
                    content_key: "oriented".into(),
                    captured_at: None,
                },
            )
            .unwrap();
            media_repo::get_by_id(&conn, media_id).unwrap().unwrap()
        };

        assert_eq!(
            item.orientation, 1,
            "the fresh database row recreates the stale-metadata window"
        );
        assert_eq!(
            source_media_orientation(&item, MediaKind::Photo, None),
            6,
            "source EXIF must win at analysis time"
        );
        let decoded = teo_media_core::decode::load_image(&photo, 6, None, None).unwrap();
        assert_eq!(decoded.dimensions(), (240, 320), "orientation 6 swaps the decoded axes");
    }

    #[test]
    fn a_missing_source_file_is_reported_not_panicked() {
        let cache = tempfile::tempdir().unwrap();
        let db = Database::open_in_memory().unwrap();
        let item = {
            let conn = db.conn().unwrap();
            let shoot = shoots::create(&conn, "Shoot", "C:\\gone").unwrap();
            let media_id = media_repo::upsert(
                &conn,
                &NewMedia {
                    shoot_id: shoot.id,
                    path: "C:\\gone\\missing.jpg".into(),
                    filename: "missing.jpg".into(),
                    media_type: MediaType::Photo,
                    extension: "jpg".into(),
                    file_size: 1,
                    content_key: "k".into(),
                    captured_at: None,
                },
            )
            .unwrap();
            media_repo::get_by_id(&conn, media_id).unwrap().unwrap()
        };

        let thumbnails = ThumbnailCache::new(cache.path());
        assert!(index_media(&db, &thumbnails, None, &item).is_err());

        let conn = db.conn().unwrap();
        assert_eq!(
            media_repo::get_by_id(&conn, item.id).unwrap().unwrap().processing_status,
            "failed"
        );
    }
}
