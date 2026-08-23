//! Shoot-level pipeline stages: scan, recognise, cluster, albums.
//!
//! These run once per shoot rather than once per file, and each one is
//! idempotent — re-running it produces the same result and never undoes a
//! human decision. That property is what makes "Resume Processing" safe.

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use serde::Serialize;
use teo_clustering::{cluster_faces, FaceMatcher};
use teo_database::models::{JobKind, MediaType, NewMedia, ProcessingStatus, ShootStatus};
use teo_database::repo::{albums, clusters, faces, jobs, logs, media as media_repo, shoots};
use teo_database::Database;
use teo_media_core::{scan, MediaKind, ScanOptions};

use crate::settings::AppSettings;

/// Job priorities. Lower numbers run first, so the queue naturally moves
/// through indexing, then per-file AI, then the shoot-wide stages.
pub mod priority {
    pub const SCAN: i64 = 10;
    pub const INDEX: i64 = 50;
    pub const ANALYSE_PHOTO: i64 = 100;
    pub const ANALYSE_VIDEO: i64 = 120;
    pub const RECOGNISE: i64 = 300;
    pub const CLUSTER: i64 = 400;
    pub const ALBUMS: i64 = 500;
}

#[derive(Debug, thiserror::Error)]
pub enum StageError {
    #[error(transparent)]
    Database(#[from] teo_database::DbError),
    #[error(transparent)]
    Media(#[from] teo_media_core::MediaError),
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, StageError>;

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanSummary {
    pub photos: usize,
    pub videos: usize,
    pub skipped: usize,
    pub new_media: usize,
    pub cancelled: bool,
}

/// Walks the shoot folder, indexes what it finds, and queues the work.
pub fn scan_shoot(
    db: &Database,
    shoot_id: i64,
    settings: &AppSettings,
    cancel: Option<Arc<AtomicBool>>,
    mut on_progress: impl FnMut(usize),
) -> Result<ScanSummary> {
    let shoot = {
        let conn = db.conn()?;
        shoots::get_by_id(&conn, shoot_id)?
            .ok_or_else(|| StageError::Other(format!("shoot {shoot_id} not found")))?
    };

    {
        let conn = db.conn()?;
        shoots::set_status(&conn, shoot_id, ShootStatus::Scanning)?;
    }

    let options = ScanOptions { recursive: settings.scan_recursive, ..Default::default() };
    let report = scan(std::path::Path::new(&shoot.source_path), &options, cancel, &mut on_progress)?;

    let mut summary = ScanSummary {
        photos: report.photos,
        videos: report.videos,
        skipped: report.skipped,
        cancelled: report.cancelled,
        new_media: 0,
    };

    // One transaction for the whole insert: on a 2,400-file shoot this is the
    // difference between a couple of seconds and a couple of minutes.
    let queued: Vec<(i64, MediaKind)> = db.transaction(|conn| {
        let mut queued = Vec::with_capacity(report.files.len());
        for file in &report.files {
            let media_id = media_repo::upsert(
                conn,
                &NewMedia {
                    shoot_id,
                    path: file.path.display().to_string(),
                    filename: file.filename.clone(),
                    media_type: match file.kind {
                        MediaKind::Photo => MediaType::Photo,
                        MediaKind::Video => MediaType::Video,
                    },
                    extension: file.extension.clone(),
                    file_size: file.file_size as i64,
                    content_key: file.content_key.clone(),
                    captured_at: file.modified_at.clone(),
                },
            )?;
            queued.push((media_id, file.kind));
        }
        Ok(queued)
    })?;

    // Queue per-file work only for files that still need it, so a re-scan of a
    // mostly-processed shoot is nearly free.
    let pending: std::collections::HashSet<i64> = {
        let conn = db.conn()?;
        media_repo::pending(&conn, shoot_id, i64::MAX)?
            .into_iter()
            .map(|m| m.id)
            .collect()
    };

    db.transaction(|conn| {
        for (media_id, kind) in &queued {
            if !pending.contains(media_id) {
                continue;
            }
            summary.new_media += 1;
            jobs::enqueue(conn, shoot_id, JobKind::Thumbnail, Some(*media_id), priority::INDEX, None)?;
            let (job_kind, job_priority) = match kind {
                MediaKind::Photo => (JobKind::AnalysePhoto, priority::ANALYSE_PHOTO),
                MediaKind::Video => (JobKind::AnalyseVideo, priority::ANALYSE_VIDEO),
            };
            jobs::enqueue(conn, shoot_id, job_kind, Some(*media_id), job_priority, None)?;
        }
        Ok(())
    })?;

    {
        let conn = db.conn()?;
        queue_finishing_stages(&conn, shoot_id)?;
        shoots::set_status(&conn, shoot_id, ShootStatus::Processing)?;
        logs::record_quiet(
            &conn,
            logs::EVENT_SHOOT_IMPORTED,
            Some(shoot_id),
            None,
            None,
            Some(&format!(
                "{} photos, {} videos, {} newly queued",
                summary.photos, summary.videos, summary.new_media
            )),
        );
    }

    Ok(summary)
}

/// Queues the three shoot-wide stages, if they are not already waiting.
pub fn queue_finishing_stages(conn: &teo_database::rusqlite::Connection, shoot_id: i64) -> Result<()> {
    jobs::enqueue_unique(conn, shoot_id, JobKind::Recognise, None, priority::RECOGNISE)?;
    jobs::enqueue_unique(conn, shoot_id, JobKind::Cluster, None, priority::CLUSTER)?;
    jobs::enqueue_unique(conn, shoot_id, JobKind::Albums, None, priority::ALBUMS)?;
    Ok(())
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecogniseReport {
    pub library_players: usize,
    pub library_samples: usize,
    pub faces_examined: usize,
    pub faces_matched: usize,
    pub faces_auto_confirmed: usize,
}

/// Compares every unidentified face against the player library (§6).
///
/// Matching happens per image rather than per face so the "one player cannot
/// appear twice in the same frame" rule can be applied.
pub fn recognise_shoot(db: &Database, shoot_id: i64, settings: &AppSettings) -> Result<RecogniseReport> {
    let (library, unassigned) = {
        let conn = db.conn()?;
        (faces::library_vectors(&conn)?, faces::unassigned_vectors(&conn, shoot_id)?)
    };

    let matcher = FaceMatcher::build(
        library
            .into_iter()
            .filter_map(|v| v.person_id.map(|person_id| (person_id, v.embedding))),
    );

    let mut report = RecogniseReport {
        library_players: matcher.player_count(),
        library_samples: matcher.total_samples(),
        faces_examined: unassigned.len(),
        ..Default::default()
    };

    if matcher.is_empty() || unassigned.is_empty() {
        // Nothing to match against yet — everything falls through to clustering,
        // which is exactly the intended behaviour for a first-ever shoot.
        return Ok(report);
    }

    // Group by image so each frame is resolved as a whole.
    let mut by_media: std::collections::BTreeMap<i64, Vec<teo_database::repo::faces::FaceVector>> =
        std::collections::BTreeMap::new();
    for vector in unassigned {
        by_media.entry(vector.media_id).or_default().push(vector);
    }

    let config = settings.matcher_config();
    let auto_confirm = settings.auto_confirm_above;

    db.transaction(|conn| {
        for (_, group) in by_media {
            let embeddings: Vec<Vec<f32>> = group.iter().map(|v| v.embedding.clone()).collect();
            for (vector, matched) in group.iter().zip(matcher.match_frame(&embeddings, &config)) {
                let Some(matched) = matched else { continue };
                report.faces_matched += 1;

                // Auto-confirmation is off by default: §10 is explicit that AI
                // results should not be treated as final without review.
                if auto_confirm < 1.0 && matched.similarity >= auto_confirm {
                    faces::assign(conn, vector.face_id, matched.person_id, Some(matched.similarity as f64))?;
                    report.faces_auto_confirmed += 1;
                } else {
                    faces::set_suggestion(conn, vector.face_id, matched.person_id, matched.similarity as f64)?;
                }
            }
        }
        Ok(())
    })?;

    // Carry the identifications through to the video timeline.
    {
        let conn = db.conn()?;
        conn.execute(
            "UPDATE video_detections SET person_id = (SELECT f.person_id FROM faces f WHERE f.id = video_detections.face_id)
              WHERE face_id IS NOT NULL
                AND media_id IN (SELECT id FROM media WHERE shoot_id = ?1)",
            teo_database::rusqlite::params![shoot_id],
        )
        .map_err(teo_database::DbError::from)?;
    }

    Ok(report)
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClusterReport {
    pub faces_clustered: usize,
    pub clusters_created: usize,
    pub faces_left_alone: usize,
}

/// Groups whatever recognition could not identify (§7).
pub fn cluster_shoot(db: &Database, shoot_id: i64, settings: &AppSettings) -> Result<ClusterReport> {
    let vectors = {
        let conn = db.conn()?;
        // Named clusters survive; only the machine-generated ones are rebuilt.
        clusters::clear_unnamed(&conn, shoot_id)?;
        faces::unassigned_vectors(&conn, shoot_id)?
    };

    if vectors.is_empty() {
        let conn = db.conn()?;
        clusters::refresh_counts(&conn, shoot_id)?;
        return Ok(ClusterReport::default());
    }

    let embeddings: Vec<Vec<f32>> = vectors.iter().map(|v| v.embedding.clone()).collect();
    let result = cluster_faces(&embeddings, &settings.cluster_config());

    let mut report = ClusterReport {
        faces_left_alone: result.unclustered.len(),
        clusters_created: result.cluster_count(),
        ..Default::default()
    };

    db.transaction(|conn| {
        for (index, cluster) in result.clusters.iter().enumerate() {
            let cluster_id = clusters::create(conn, shoot_id, &format!("Unknown Person {}", index + 1))?;
            for member in &cluster.members {
                if let Some(vector) = vectors.get(*member) {
                    faces::set_cluster(conn, vector.face_id, Some(cluster_id))?;
                    report.faces_clustered += 1;
                }
            }
        }
        // Faces too isolated to group keep no stale cluster from a previous run.
        for index in &result.unclustered {
            if let Some(vector) = vectors.get(*index) {
                faces::set_cluster(conn, vector.face_id, None)?;
            }
        }
        Ok(())
    })?;

    {
        let conn = db.conn()?;
        clusters::refresh_counts(&conn, shoot_id)?;
    }

    Ok(report)
}

/// Rebuilds the shoot's albums and marks it complete.
pub fn generate_albums(db: &Database, shoot_id: i64) -> Result<usize> {
    let created = db.transaction(|conn| albums::regenerate(conn, shoot_id))?;

    let conn = db.conn()?;
    let progress = jobs::progress(&conn, shoot_id)?;
    let status = if progress.media_failed > 0 && progress.media_analysed == 0 {
        ShootStatus::Failed
    } else {
        ShootStatus::Completed
    };
    shoots::set_status(&conn, shoot_id, status)?;
    Ok(created)
}

/// Throws away every derived result for a shoot so it can be analysed again
/// from scratch — used after changing a model or a threshold.
pub fn reset_analysis(db: &Database, shoot_id: i64) -> Result<()> {
    db.transaction(|conn| {
        conn.execute(
            "DELETE FROM faces WHERE shoot_id = ?1",
            teo_database::rusqlite::params![shoot_id],
        )?;
        conn.execute(
            "DELETE FROM video_detections WHERE media_id IN (SELECT id FROM media WHERE shoot_id = ?1)",
            teo_database::rusqlite::params![shoot_id],
        )?;
        conn.execute(
            "DELETE FROM clusters WHERE shoot_id = ?1",
            teo_database::rusqlite::params![shoot_id],
        )?;
        conn.execute(
            "DELETE FROM albums WHERE shoot_id = ?1",
            teo_database::rusqlite::params![shoot_id],
        )?;
        // `media_groups` is deliberately left alone: the editor's own sorting is
        // not an AI result and must survive a re-analysis.
        media_repo::reset_analysis(conn, shoot_id)?;
        Ok(())
    })?;

    let conn = db.conn()?;
    jobs::cancel_for_shoot(&conn, shoot_id)?;
    jobs::clear_finished(&conn, shoot_id)?;
    Ok(())
}

/// Queues analysis for anything in the shoot that is not finished.
pub fn queue_pending_work(db: &Database, shoot_id: i64) -> Result<usize> {
    let pending = {
        let conn = db.conn()?;
        media_repo::pending(&conn, shoot_id, i64::MAX)?
    };

    let queued = db.transaction(|conn| {
        let mut count = 0;
        for item in &pending {
            if item.processing_status == ProcessingStatus::Pending.as_str()
                || item.thumbnail_path.is_none()
            {
                jobs::enqueue_unique(conn, shoot_id, JobKind::Thumbnail, Some(item.id), priority::INDEX)?;
            }
            let kind = if item.media_type == MediaType::Video.as_str() {
                JobKind::AnalyseVideo
            } else {
                JobKind::AnalysePhoto
            };
            let job_priority = if item.media_type == MediaType::Video.as_str() {
                priority::ANALYSE_VIDEO
            } else {
                priority::ANALYSE_PHOTO
            };
            if jobs::enqueue_unique(conn, shoot_id, kind, Some(item.id), job_priority)?.is_some() {
                count += 1;
            }
        }
        Ok(count)
    })?;

    {
        let conn = db.conn()?;
        queue_finishing_stages(&conn, shoot_id)?;
        shoots::set_status(&conn, shoot_id, ShootStatus::Processing)?;
    }

    Ok(queued)
}

#[cfg(test)]
mod tests {
    use super::*;
    use teo_database::models::BoundingBox;
    use teo_database::repo::people;

    fn seed_shoot(db: &Database) -> i64 {
        let conn = db.conn().unwrap();
        shoots::create(&conn, "Test Shoot", "C:\\shoot").unwrap().id
    }

    fn add_face(db: &Database, shoot_id: i64, filename: &str, embedding: Vec<f32>) -> (i64, i64) {
        let conn = db.conn().unwrap();
        let media_id = media_repo::upsert(
            &conn,
            &NewMedia {
                shoot_id,
                path: format!("C:\\shoot\\{filename}"),
                filename: filename.to_string(),
                media_type: MediaType::Photo,
                extension: "jpg".into(),
                file_size: 1,
                content_key: filename.to_string(),
                captured_at: None,
            },
        )
        .unwrap();
        let face_id = faces::insert(
            &conn,
            &teo_database::models::NewFace {
                media_id,
                shoot_id,
                bbox: BoundingBox { x: 0.1, y: 0.1, w: 0.2, h: 0.2 },
                landmarks: None,
                detection_confidence: 0.95,
                embedding: Some(embedding),
                quality: Some(0.7),
                frame_time: None,
                crop_path: None,
            },
        )
        .unwrap();
        (media_id, face_id)
    }

    fn unit(v: Vec<f32>) -> Vec<f32> {
        let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        v.into_iter().map(|x| x / norm).collect()
    }

    #[test]
    fn recognition_suggests_rather_than_confirms_by_default() {
        let db = Database::open_in_memory().unwrap();
        let shoot_id = seed_shoot(&db);

        // A known player, established by a confirmed face in an earlier shoot.
        let (_, known_face) = add_face(&db, shoot_id, "known.jpg", unit(vec![1.0, 0.0, 0.0]));
        {
            let conn = db.conn().unwrap();
            let person = people::get_or_create(&conn, "Jonathan", None).unwrap();
            faces::assign(&conn, known_face, person.id, Some(1.0)).unwrap();
        }

        let (_, new_face) = add_face(&db, shoot_id, "new.jpg", unit(vec![0.97, 0.1, 0.0]));

        let report = recognise_shoot(&db, shoot_id, &AppSettings::default()).unwrap();
        assert_eq!(report.library_players, 1);
        assert_eq!(report.faces_matched, 1);
        assert_eq!(report.faces_auto_confirmed, 0, "nothing is auto-confirmed by default");

        let conn = db.conn().unwrap();
        let face = faces::get_by_id(&conn, new_face).unwrap().unwrap();
        assert_eq!(face.assignment, "suggested");
        assert!(face.person_id.is_some());
    }

    #[test]
    fn auto_confirm_applies_above_its_threshold() {
        let db = Database::open_in_memory().unwrap();
        let shoot_id = seed_shoot(&db);
        let (_, known_face) = add_face(&db, shoot_id, "known.jpg", unit(vec![1.0, 0.0, 0.0]));
        {
            let conn = db.conn().unwrap();
            let person = people::get_or_create(&conn, "Jonathan", None).unwrap();
            faces::assign(&conn, known_face, person.id, Some(1.0)).unwrap();
        }
        let (_, new_face) = add_face(&db, shoot_id, "new.jpg", unit(vec![0.999, 0.02, 0.0]));

        let settings = AppSettings { auto_confirm_above: 0.9, ..Default::default() };
        let report = recognise_shoot(&db, shoot_id, &settings).unwrap();
        assert_eq!(report.faces_auto_confirmed, 1);

        let conn = db.conn().unwrap();
        assert_eq!(faces::get_by_id(&conn, new_face).unwrap().unwrap().assignment, "confirmed");
    }

    #[test]
    fn an_empty_library_leaves_everything_for_clustering() {
        let db = Database::open_in_memory().unwrap();
        let shoot_id = seed_shoot(&db);
        add_face(&db, shoot_id, "a.jpg", unit(vec![1.0, 0.0, 0.0]));

        let report = recognise_shoot(&db, shoot_id, &AppSettings::default()).unwrap();
        assert_eq!(report.library_players, 0);
        assert_eq!(report.faces_matched, 0);
    }

    #[test]
    fn clustering_groups_unknown_faces_and_names_them_in_order() {
        let db = Database::open_in_memory().unwrap();
        let shoot_id = seed_shoot(&db);

        for i in 0..5 {
            add_face(&db, shoot_id, &format!("a{i}.jpg"), unit(vec![1.0, 0.02 * i as f32, 0.0]));
        }
        for i in 0..3 {
            add_face(&db, shoot_id, &format!("b{i}.jpg"), unit(vec![0.0, 0.02 * i as f32, 1.0]));
        }

        let report = cluster_shoot(&db, shoot_id, &AppSettings::default()).unwrap();
        assert_eq!(report.clusters_created, 2);
        assert_eq!(report.faces_clustered, 8);

        let conn = db.conn().unwrap();
        let summaries = clusters::list_summaries(&conn, shoot_id, false).unwrap();
        assert_eq!(summaries[0].cluster.label, "Unknown Person 1");
        assert_eq!(summaries[0].cluster.face_count, 5);
        assert_eq!(summaries[1].cluster.face_count, 3);
    }

    #[test]
    fn reclustering_preserves_a_named_cluster() {
        let db = Database::open_in_memory().unwrap();
        let shoot_id = seed_shoot(&db);
        for i in 0..5 {
            add_face(&db, shoot_id, &format!("a{i}.jpg"), unit(vec![1.0, 0.02 * i as f32, 0.0]));
        }

        cluster_shoot(&db, shoot_id, &AppSettings::default()).unwrap();

        let named_id = {
            let conn = db.conn().unwrap();
            let summaries = clusters::list_summaries(&conn, shoot_id, false).unwrap();
            let person = people::get_or_create(&conn, "Jelly", None).unwrap();
            clusters::name_cluster(&conn, summaries[0].cluster.id, person.id).unwrap();
            summaries[0].cluster.id
        };

        // Re-running must not undo the identification.
        cluster_shoot(&db, shoot_id, &AppSettings::default()).unwrap();

        let conn = db.conn().unwrap();
        let cluster = clusters::get_by_id(&conn, named_id).unwrap().expect("named cluster survives");
        assert_eq!(cluster.status, "named");
        assert_eq!(faces::library_vectors(&conn).unwrap().len(), 5);
    }

    #[test]
    fn albums_are_generated_and_the_shoot_completes() {
        let db = Database::open_in_memory().unwrap();
        let shoot_id = seed_shoot(&db);
        let (_, face_id) = add_face(&db, shoot_id, "a.jpg", unit(vec![1.0, 0.0]));
        {
            let conn = db.conn().unwrap();
            let person = people::get_or_create(&conn, "Mavi", None).unwrap();
            faces::assign(&conn, face_id, person.id, Some(0.99)).unwrap();
        }

        assert!(generate_albums(&db, shoot_id).unwrap() >= 1);

        let conn = db.conn().unwrap();
        assert_eq!(shoots::get_by_id(&conn, shoot_id).unwrap().unwrap().status, "completed");
        assert!(albums::list(&conn, shoot_id).unwrap().iter().any(|a| a.name == "Mavi"));
    }

    #[test]
    fn reset_clears_derived_data_but_keeps_the_media_index() {
        let db = Database::open_in_memory().unwrap();
        let shoot_id = seed_shoot(&db);
        add_face(&db, shoot_id, "a.jpg", unit(vec![1.0, 0.0]));
        cluster_shoot(&db, shoot_id, &AppSettings::default()).unwrap();
        generate_albums(&db, shoot_id).unwrap();

        reset_analysis(&db, shoot_id).unwrap();

        let conn = db.conn().unwrap();
        assert_eq!(media_repo::count_for_shoot(&conn, shoot_id).unwrap(), 1, "the file index survives");
        assert!(faces::for_media(&conn, 1).unwrap().is_empty());
        assert!(albums::list(&conn, shoot_id).unwrap().is_empty());
        assert!(clusters::list_summaries(&conn, shoot_id, true).unwrap().is_empty());
    }
}
