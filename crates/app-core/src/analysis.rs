//! What per-file analysis produces, separated from where it is written.
//!
//! The GPU half of the pipeline — decode, detect, embed — can run on any
//! machine that has the models. The database half cannot: clients hold no
//! credentials, and the server is the only writer. So [`Engine`](crate::pipeline::Engine)
//! *computes* an [`AnalysisOutput`], and [`apply_analysis`] writes it. On one
//! box the two run back to back; across the network the output travels as
//! JSON (the review frames as separate byte uploads) and the server applies
//! it exactly as it would its own.

use serde::{Deserialize, Serialize};
use skwad_database::models::{BoundingBox, Media, MediaType, NewFace, ProcessingStatus};
use skwad_database::repo::{faces, media as media_repo, video as video_repo};
use skwad_database::Database;
use skwad_media_core::VideoFrameCache;

use crate::pipeline::{AnalysisOutcome, PipelineError, Result};

/// One detected face, in the portable form that goes into a `faces` row.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FaceRecord {
    /// Normalised against the oriented frame, like every stored box.
    pub bbox: BoundingBox,
    /// Five landmarks in pixel space, `[x0, y0, …, x4, y4]`.
    pub landmarks: Option<Vec<f32>>,
    pub detection_confidence: f64,
    pub embedding: Option<Vec<f32>>,
    pub quality: f64,
    /// Seconds into the video the frame came from; `None` for a photo.
    pub frame_time: Option<f64>,
}

/// One analysed video frame that had faces in it, as the JPEG the reviewer
/// will be shown. Carried separately from the JSON on the wire.
#[derive(Debug, Clone)]
pub struct ReviewFrame {
    pub timestamp: f64,
    pub jpeg: Vec<u8>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalysisOutput {
    /// The orientation read from the source when it differed from the
    /// indexed value, so the row is corrected before boxes are stored.
    pub orientation: Option<i64>,
    pub faces: Vec<FaceRecord>,
    /// Every video frame that was decoded, faces or not.
    pub sample_times: Vec<f64>,
    /// Set when the file was deliberately not analysed (video analysis off);
    /// the row is marked skipped with this reason.
    pub skipped: Option<String>,
    /// Content hash of the embedder, recorded on every face row.
    pub embedder_key: String,
    /// Distinct frames looked at. 1 for a photo.
    pub frames_analysed: usize,
    #[serde(skip)]
    pub review_frames: Vec<ReviewFrame>,
}

impl AnalysisOutput {
    pub fn outcome(&self) -> AnalysisOutcome {
        AnalysisOutcome {
            faces_detected: self.faces.len(),
            faces_embedded: self.faces.iter().filter(|f| f.embedding.is_some()).count(),
            frames_analysed: self.frames_analysed,
        }
    }
}

/// Writes an analysis into the library. Re-analysis replaces, never appends:
/// the file's previous detections, timeline entries, sample frames and cached
/// review frames all go first.
pub fn apply_analysis(
    db: &Database,
    video_frames: &VideoFrameCache,
    item: &Media,
    output: AnalysisOutput,
) -> Result<AnalysisOutcome> {
    if let Some(reason) = &output.skipped {
        let mut conn = db.conn()?;
        media_repo::set_status(&mut conn, item.id, ProcessingStatus::Skipped, Some(reason))?;
        return Ok(AnalysisOutcome::default());
    }

    let is_video = item.media_type == MediaType::Video.as_str();
    if let Some(orientation) = output.orientation {
        if orientation != item.orientation {
            tracing::warn!(
                media = item.id,
                indexed = item.orientation,
                source = orientation,
                "correcting stale orientation before storing face analysis"
            );
            let mut conn = db.conn()?;
            media_repo::set_orientation(&mut conn, item.id, orientation)?;
        }
    }

    {
        let mut conn = db.conn()?;
        faces::delete_for_media(&mut conn, item.id)?;
        if is_video {
            video_repo::delete_for_media(&mut conn, item.id)?;
            video_repo::delete_sample_frames(&mut conn, item.id)?;
        }
    }
    if is_video {
        if let Err(error) = video_frames.remove(&item.content_key) {
            tracing::warn!(video = %item.filename, %error, "could not clear stale review frames");
        }
    }

    let outcome = output.outcome();
    db.transaction(|conn| {
        for at in &output.sample_times {
            video_repo::insert_sample_frame(conn, item.id, *at)?;
        }
        for face in &output.faces {
            let face_id = faces::insert(
                conn,
                &NewFace {
                    media_id: item.id,
                    shoot_id: item.shoot_id,
                    bbox: face.bbox,
                    landmarks: face.landmarks.clone(),
                    detection_confidence: face.detection_confidence,
                    embedding: face.embedding.clone(),
                    quality: Some(face.quality),
                    frame_time: face.frame_time,
                    crop_path: None,
                    model_key: Some(output.embedder_key.clone()),
                },
            )?;
            // Videos additionally get a timeline entry, filled in with a
            // person once recognition runs.
            if let Some(at) = face.frame_time {
                video_repo::insert(conn, item.id, None, Some(face_id), at, face.detection_confidence)?;
            }
        }
        Ok(())
    })?;

    for frame in &output.review_frames {
        if let Err(error) = video_frames.store_encoded(&frame.jpeg, &item.content_key, frame.timestamp) {
            tracing::warn!(video = %item.filename, at = frame.timestamp, %error, "could not cache tagged review frame");
        }
    }

    {
        let mut conn = db.conn()?;
        media_repo::set_status(&mut conn, item.id, ProcessingStatus::Analysed, None)?;
        media_repo::refresh_face_count(&mut conn, item.id)?;
    }
    Ok(outcome)
}

/// A JSON round trip must not lose anything the database needs.
pub fn check_portable(output: &AnalysisOutput) -> Result<()> {
    let json = serde_json::to_string(output).map_err(|e| PipelineError::Other(e.to_string()))?;
    let _: AnalysisOutput = serde_json::from_str(&json).map_err(|e| PipelineError::Other(e.to_string()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use skwad_database::models::NewMedia;
    use skwad_database::repo::shoots;

    fn video_row(db: &Database) -> Media {
        let mut conn = db.conn().unwrap();
        let shoot = shoots::create(&mut conn, "S", "C:\\s").unwrap();
        let id = media_repo::upsert(
            &mut conn,
            &NewMedia {
                shoot_id: shoot.id,
                path: "C:\\s\\clip.mp4".into(),
                filename: "clip.mp4".into(),
                media_type: MediaType::Video,
                extension: "mp4".into(),
                file_size: 1,
                content_key: "clipkey".into(),
                captured_at: None,
                normalized_relative_path: Some("clip.mp4".into()),
            },
        )
        .unwrap();
        media_repo::get_by_id(&mut conn, id).unwrap().unwrap()
    }

    #[test]
    fn an_output_applies_the_way_the_inline_pipeline_wrote() {
        let db = Database::open_test().unwrap();
        let cache_dir = tempfile::tempdir().unwrap();
        let cache = VideoFrameCache::new(cache_dir.path());
        let item = video_row(&db);

        let output = AnalysisOutput {
            orientation: Some(6),
            faces: vec![
                FaceRecord {
                    bbox: BoundingBox { x: 0.1, y: 0.1, w: 0.2, h: 0.2 },
                    landmarks: Some(vec![1.0; 10]),
                    detection_confidence: 0.9,
                    embedding: Some(vec![0.5, 0.5]),
                    quality: 0.7,
                    frame_time: Some(2.0),
                },
                FaceRecord {
                    bbox: BoundingBox { x: 0.5, y: 0.1, w: 0.2, h: 0.2 },
                    landmarks: None,
                    detection_confidence: 0.8,
                    embedding: None,
                    quality: 0.4,
                    frame_time: Some(7.0),
                },
            ],
            sample_times: vec![2.0, 7.0, 12.0],
            skipped: None,
            embedder_key: "model-a".into(),
            frames_analysed: 3,
            review_frames: vec![ReviewFrame { timestamp: 2.0, jpeg: vec![0xff, 0xd8, 0xff] }],
        };
        check_portable(&output).unwrap();

        let outcome = apply_analysis(&db, &cache, &item, output).unwrap();
        assert_eq!(outcome.faces_detected, 2);
        assert_eq!(outcome.faces_embedded, 1);

        let mut conn = db.conn().unwrap();
        let stored = media_repo::get_by_id(&mut conn, item.id).unwrap().unwrap();
        assert_eq!(stored.processing_status, "analysed");
        assert_eq!(stored.orientation, 6, "the source orientation corrected the row");
        assert_eq!(stored.face_count, 2);
        let faces = faces::for_media(&mut conn, item.id).unwrap();
        assert!(faces.iter().all(|f| f.model_key.as_deref() == Some("model-a")));
        assert_eq!(video_repo::sample_times(&mut conn, item.id).unwrap(), vec![2.0, 7.0, 12.0]);
        assert_eq!(video_repo::timelines(&mut conn, item.id).unwrap().len(), 1, "one unassigned timeline");
        assert_eq!(cache.read("clipkey", 2.0).unwrap(), Some(vec![0xff, 0xd8, 0xff]));

        // Applying again replaces rather than appends.
        let again = AnalysisOutput {
            embedder_key: "model-a".into(),
            frames_analysed: 1,
            ..Default::default()
        };
        apply_analysis(&db, &cache, &item, again).unwrap();
        assert!(faces::for_media(&mut conn, item.id).unwrap().is_empty());
        assert_eq!(cache.read("clipkey", 2.0).unwrap(), None, "stale review frames are cleared");
    }

    #[test]
    fn a_skipped_output_marks_the_row_and_writes_nothing_else() {
        let db = Database::open_test().unwrap();
        let cache_dir = tempfile::tempdir().unwrap();
        let cache = VideoFrameCache::new(cache_dir.path());
        let item = video_row(&db);
        let output = AnalysisOutput {
            skipped: Some("video analysis is off".into()),
            ..Default::default()
        };
        apply_analysis(&db, &cache, &item, output).unwrap();
        let mut conn = db.conn().unwrap();
        let stored = media_repo::get_by_id(&mut conn, item.id).unwrap().unwrap();
        assert_eq!(stored.processing_status, "skipped");
        assert_eq!(stored.error.as_deref(), Some("video analysis is off"));
    }
}
