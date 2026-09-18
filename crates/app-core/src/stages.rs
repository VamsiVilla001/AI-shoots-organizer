//! Shoot-level pipeline stages: scan, recognise, cluster, albums.
//!
//! These run once per shoot rather than once per file, and each one is
//! idempotent — re-running it produces the same result and never undoes a
//! human decision. That property is what makes "Resume Processing" safe.

use skwad_database::Db;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use serde::Serialize;
use skwad_clustering::{cluster_faces, FaceMatcher};
use skwad_database::models::{JobKind, MediaType, NewMedia, ProcessingStatus, ShootStatus};
use skwad_database::repo::{albums, clusters, faces, jobs, logs, media as media_repo, shoots};
use skwad_database::Database;
use skwad_media_core::{scan, MediaKind, ScanOptions};

use crate::settings::AppSettings;

/// Bound each SQLite write transaction so UI reads and progress updates get a
/// regular chance to run during very large imports.
const SCAN_DB_BATCH_SIZE: usize = 200;
/// Tiny/background faces make unstable identity references. Always retain the
/// best confirmed sample for a player, then admit only useful-quality extras.
const MIN_REFERENCE_QUALITY: f64 = 0.55;
const MAX_REFERENCE_SAMPLES_PER_PERSON: usize = 8;
/// Also the cap on reference samples kept from an enrollment video (`commands::enroll_person`).
pub const MAX_ENROLLMENT_VIDEO_SAMPLES: usize = MAX_REFERENCE_SAMPLES_PER_PERSON;

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
    Database(#[from] skwad_database::DbError),
    #[error(transparent)]
    Media(#[from] skwad_media_core::MediaError),
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
        let mut conn = db.conn()?;
        shoots::get_by_id(&mut conn, shoot_id)?
            .ok_or_else(|| StageError::Other(format!("shoot {shoot_id} not found")))?
    };

    {
        let mut conn = db.conn()?;
        shoots::set_status(&mut conn, shoot_id, ShootStatus::Scanning)?;
    }

    let options = ScanOptions {
        recursive: settings.scan_recursive,
        ..Default::default()
    };
    let report = scan(
        std::path::Path::new(&shoot.source_path),
        &options,
        cancel,
        &mut on_progress,
    )?;

    let mut summary = ScanSummary {
        photos: report.photos,
        videos: report.videos,
        skipped: report.skipped,
        cancelled: report.cancelled,
        new_media: 0,
    };

    // Batch inserts rather than holding one writer lock for an entire large
    // shoot. This keeps the UI and progress monitor responsive without paying
    // the cost of one transaction per file.
    let source_root = std::path::PathBuf::from(&shoot.source_path);
    let mut queued: Vec<(i64, MediaKind)> = Vec::with_capacity(report.files.len());
    for batch in report.files.chunks(SCAN_DB_BATCH_SIZE) {
        let mut rows = db.transaction(|conn| {
            let mut rows = Vec::with_capacity(batch.len());
            for file in batch {
                // The half of the path that survives a change of machine. The
                // absolute `path` stays exactly as scanned because `content_key`
                // is derived from it; see `skwad_database::paths`.
                let normalized_relative_path = skwad_database::paths::relative_to_root(&source_root, &file.path);
                if normalized_relative_path.is_none() {
                    tracing::warn!(
                        file = %file.path.display(),
                        root = %shoot.source_path,
                        "scanned file is not under the shoot root; it will not be reachable from other machines"
                    );
                }
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
                        normalized_relative_path,
                    },
                )?;
                rows.push((media_id, file.kind));
            }
            Ok(rows)
        })?;
        queued.append(&mut rows);
    }

    // Queue per-file work only for files that still need it, so a re-scan of a
    // mostly-processed shoot is nearly free.
    let pending: std::collections::HashSet<i64> = {
        let mut conn = db.conn()?;
        media_repo::pending(&mut conn, shoot_id, i64::MAX)?
            .into_iter()
            .map(|m| m.id)
            .collect()
    };

    for batch in queued.chunks(SCAN_DB_BATCH_SIZE) {
        let added = db.transaction(|conn| {
            let mut added = 0usize;
            for (media_id, kind) in batch {
                if !pending.contains(media_id) {
                    continue;
                }
                added += 1;
                jobs::enqueue(
                    conn,
                    shoot_id,
                    JobKind::Thumbnail,
                    Some(*media_id),
                    priority::INDEX,
                    None,
                )?;
                let (job_kind, job_priority) = match kind {
                    MediaKind::Photo => (JobKind::AnalysePhoto, priority::ANALYSE_PHOTO),
                    MediaKind::Video => (JobKind::AnalyseVideo, priority::ANALYSE_VIDEO),
                };
                jobs::enqueue(conn, shoot_id, job_kind, Some(*media_id), job_priority, None)?;
            }
            Ok(added)
        })?;
        summary.new_media += added;
    }

    {
        let mut conn = db.conn()?;
        queue_finishing_stages(&mut conn, shoot_id)?;
        shoots::set_status(&mut conn, shoot_id, ShootStatus::Processing)?;
        logs::record_quiet(
            &mut conn,
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
pub fn queue_finishing_stages(conn: &mut dyn skwad_database::Db, shoot_id: i64) -> Result<()> {
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

/// Vectors split by the embedder that produced them. Two cohorts live in two
/// different vector spaces, so every comparison — matching or clustering —
/// happens inside one cohort and never across.
type Cohorts = std::collections::BTreeMap<Option<String>, Vec<skwad_database::repo::faces::FaceVector>>;

fn by_cohort(vectors: Vec<skwad_database::repo::faces::FaceVector>) -> Cohorts {
    let mut cohorts = Cohorts::new();
    for vector in vectors {
        cohorts.entry(vector.model_key.clone()).or_default().push(vector);
    }
    cohorts
}

/// Groups vectors by the exact frame they came from, so each sampled
/// timestamp of a video is resolved independently. Grouping only by media id
/// treated an entire video as one group photo and prevented the same player
/// from matching at more than one sampled timestamp.
fn by_frame(
    vectors: Vec<skwad_database::repo::faces::FaceVector>,
) -> std::collections::BTreeMap<(i64, Option<u64>), Vec<skwad_database::repo::faces::FaceVector>> {
    let mut frames = std::collections::BTreeMap::new();
    for vector in vectors {
        frames
            .entry((vector.media_id, vector.frame_time.map(f64::to_bits)))
            .or_insert_with(Vec::new)
            .push(vector);
    }
    frames
}

/// Compares every unidentified face against the player library (§6).
///
/// Matching happens per image rather than per face so the "one player cannot
/// appear twice in the same frame" rule can be applied, and per embedder
/// cohort so vectors from two different models are never compared.
pub fn recognise_shoot(db: &Database, shoot_id: i64, settings: &AppSettings) -> Result<RecogniseReport> {
    let (library, unassigned) = {
        let mut conn = db.conn()?;
        faces::clear_suggestions_for_shoot(&mut conn, shoot_id)?;
        (
            select_reference_vectors(faces::library_vectors(&mut conn)?),
            faces::unassigned_vectors(&mut conn, shoot_id)?,
        )
    };

    let mut report = RecogniseReport {
        faces_examined: unassigned.len(),
        ..Default::default()
    };

    let mut library = by_cohort(library);
    let config = settings.matcher_config();
    let auto_confirm = settings.auto_confirm_above;

    for (cohort, unassigned) in by_cohort(unassigned) {
        let matcher = FaceMatcher::build(
            library
                .remove(&cohort)
                .unwrap_or_default()
                .into_iter()
                .filter_map(|v| v.person_id.map(|person_id| (person_id, v.embedding))),
        );
        report.library_players += matcher.player_count();
        report.library_samples += matcher.total_samples();
        if matcher.is_empty() {
            // Nothing to match against in this cohort — everything falls
            // through to clustering, which is exactly the intended behaviour
            // for a first-ever shoot (and for a freshly changed model).
            continue;
        }

        db.transaction(|conn| {
            for (_, group) in by_frame(unassigned) {
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
    }
    // Library players in cohorts with nothing to match still count as known.
    for remaining in library.into_values() {
        let matcher = FaceMatcher::build(
            remaining
                .into_iter()
                .filter_map(|v| v.person_id.map(|person_id| (person_id, v.embedding))),
        );
        report.library_players += matcher.player_count();
        report.library_samples += matcher.total_samples();
    }
    if report.faces_matched == 0 {
        return Ok(report);
    }

    // Carry the identifications through to the video timeline.
    {
        let mut conn = db.conn()?;
        conn.exec(
            "UPDATE video_detections SET person_id = (SELECT f.person_id FROM faces f WHERE f.id = video_detections.face_id)
              WHERE face_id IS NOT NULL
                AND media_id IN (SELECT id FROM media WHERE shoot_id = $1)",
            skwad_database::params![shoot_id],
        )?;
    }

    Ok(report)
}

fn select_reference_vectors(
    vectors: Vec<skwad_database::repo::faces::FaceVector>,
) -> Vec<skwad_database::repo::faces::FaceVector> {
    let mut by_person = std::collections::BTreeMap::<i64, Vec<_>>::new();
    for vector in vectors {
        if let Some(person_id) = vector.person_id {
            by_person.entry(person_id).or_default().push(vector);
        }
    }

    let mut selected = Vec::new();
    for (_, mut samples) in by_person {
        samples.sort_by(|a, b| b.quality.total_cmp(&a.quality).then_with(|| a.face_id.cmp(&b.face_id)));
        let mut accepted = 0usize;
        for (index, sample) in samples.into_iter().enumerate() {
            if index == 0 || sample.quality >= MIN_REFERENCE_QUALITY {
                selected.push(sample);
                accepted += 1;
                if accepted >= MAX_REFERENCE_SAMPLES_PER_PERSON {
                    break;
                }
            }
        }
    }
    selected
}

/// Matches one person's confirmed reference samples against media that
/// predates their enrollment — the on-demand "Find media" action for a
/// pre-registered person, as opposed to `recognise_shoot`'s whole-library
/// pass that runs automatically on every newly scanned shoot.
///
/// Unlike `recognise_shoot` this never clears existing suggestions for other
/// people: it only ever calls `set_suggestion`, which itself refuses to
/// overwrite a human's `confirmed` decision, so this is safe to call
/// repeatedly (e.g. once per shoot, whenever the user asks).
pub fn match_person_in_shoot(db: &Database, shoot_id: i64, person_id: i64, settings: &AppSettings) -> Result<usize> {
    let (reference, unassigned) = {
        let mut conn = db.conn()?;
        (
            faces::reference_vectors_for_person(&mut conn, person_id)?,
            faces::unassigned_vectors(&mut conn, shoot_id)?,
        )
    };
    if reference.is_empty() || unassigned.is_empty() {
        return Ok(0);
    }

    let mut reference = by_cohort(reference);
    let config = settings.matcher_config();
    let mut new_suggestions = 0usize;
    for (cohort, unassigned) in by_cohort(unassigned) {
        let Some(samples) = reference.remove(&cohort) else { continue };
        let matcher = FaceMatcher::build(samples.into_iter().map(|v| (person_id, v.embedding)));
        db.transaction(|conn| {
            for (_, group) in by_frame(unassigned) {
                let embeddings: Vec<Vec<f32>> = group.iter().map(|v| v.embedding.clone()).collect();
                for (vector, matched) in group.iter().zip(matcher.match_frame(&embeddings, &config)) {
                    let Some(matched) = matched else { continue };
                    faces::set_suggestion(conn, vector.face_id, matched.person_id, matched.similarity as f64)?;
                    new_suggestions += 1;
                }
            }
            Ok(())
        })?;
    }

    Ok(new_suggestions)
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
        let mut conn = db.conn()?;
        // Named clusters survive; only the machine-generated ones are rebuilt.
        clusters::clear_unnamed(&mut conn, shoot_id)?;
        faces::unassigned_vectors(&mut conn, shoot_id)?
    };

    if vectors.is_empty() {
        let mut conn = db.conn()?;
        clusters::refresh_counts(&mut conn, shoot_id)?;
        return Ok(ClusterReport::default());
    }

    // One clustering pass per embedder cohort: two models' vectors are not
    // comparable, so a face embedded under an older model can only ever group
    // with other faces from that model. Cluster numbering runs on across
    // cohorts so labels stay unique within the shoot.
    let mut report = ClusterReport::default();
    let mut next_label = 1usize;
    for (_, vectors) in by_cohort(vectors) {
        let embeddings: Vec<Vec<f32>> = vectors.iter().map(|v| v.embedding.clone()).collect();
        let result = cluster_faces(&embeddings, &settings.cluster_config());
        report.faces_left_alone += result.unclustered.len();
        report.clusters_created += result.cluster_count();

        db.transaction(|conn| {
            for cluster in &result.clusters {
                let cluster_id = clusters::create(conn, shoot_id, &format!("Unknown Person {next_label}"))?;
                next_label += 1;
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
    }

    {
        let mut conn = db.conn()?;
        clusters::refresh_counts(&mut conn, shoot_id)?;
    }

    Ok(report)
}

/// Rebuilds the shoot's albums and marks it complete.
pub fn generate_albums(db: &Database, shoot_id: i64) -> Result<usize> {
    let created = db.transaction(|conn| {
        media_repo::refresh_duplicate_groups(conn, shoot_id, 6)?;
        albums::regenerate(conn, shoot_id)
    })?;

    let mut conn = db.conn()?;
    let progress = jobs::progress(&mut conn, shoot_id)?;
    let status = if progress.media_failed > 0 && progress.media_analysed == 0 {
        ShootStatus::Failed
    } else {
        ShootStatus::Completed
    };
    shoots::set_status(&mut conn, shoot_id, status)?;
    Ok(created)
}

/// Throws away every derived result for a shoot so it can be analysed again
/// from scratch — used after changing a model or a threshold.
pub fn reset_analysis(db: &Database, shoot_id: i64) -> Result<()> {
    db.transaction(|conn| {
        conn.exec(
            "DELETE FROM faces WHERE shoot_id = $1",
            skwad_database::params![shoot_id],
        )?;
        conn.exec(
            "DELETE FROM video_detections WHERE media_id IN (SELECT id FROM media WHERE shoot_id = $1)",
            skwad_database::params![shoot_id],
        )?;
        conn.exec(
            "DELETE FROM clusters WHERE shoot_id = $1",
            skwad_database::params![shoot_id],
        )?;
        conn.exec(
            "DELETE FROM albums WHERE shoot_id = $1",
            skwad_database::params![shoot_id],
        )?;
        // `media_groups` is deliberately left alone: the editor's own sorting is
        // not an AI result and must survive a re-analysis.
        media_repo::reset_analysis(conn, shoot_id)?;
        Ok(())
    })?;

    let mut conn = db.conn()?;
    jobs::cancel_for_shoot(&mut conn, shoot_id)?;
    jobs::clear_finished(&mut conn, shoot_id)?;
    Ok(())
}

/// What [`queue_reembed`] scheduled.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReembedQueued {
    pub media: usize,
    pub shoots: Vec<i64>,
}

/// Requeues analysis for every file whose embeddings came from an embedder
/// other than `current_key`, plus the finishing stages of each affected
/// shoot. Re-analysis replaces a file's detected faces, so the stale vectors
/// are gone once the job runs; reviewer-drawn faces are kept but their
/// vectors stay in the old cohort until redrawn.
pub fn queue_reembed(db: &Database, current_key: &str, shoot_id: Option<i64>) -> Result<ReembedQueued> {
    let stale = {
        let mut conn = db.conn()?;
        faces::media_with_stale_embeddings(&mut conn, current_key, shoot_id)?
    };
    let mut queued = ReembedQueued::default();
    for batch in stale.chunks(SCAN_DB_BATCH_SIZE) {
        let added = db.transaction(|conn| {
            let mut added = 0usize;
            for (shoot_id, media_id) in batch {
                let Some(item) = media_repo::get_by_id(conn, *media_id)? else { continue };
                let (kind, job_priority) = if item.media_type == MediaType::Video.as_str() {
                    (JobKind::AnalyseVideo, priority::ANALYSE_VIDEO)
                } else {
                    (JobKind::AnalysePhoto, priority::ANALYSE_PHOTO)
                };
                if jobs::enqueue_unique(conn, *shoot_id, kind, Some(*media_id), job_priority)?.is_some() {
                    added += 1;
                }
            }
            Ok(added)
        })?;
        queued.media += added;
    }
    let mut shoots: Vec<i64> = stale.iter().map(|(shoot, _)| *shoot).collect();
    shoots.dedup();
    {
        let mut conn = db.conn()?;
        for shoot in &shoots {
            queue_finishing_stages(&mut conn, *shoot)?;
            shoots::set_status(&mut conn, *shoot, ShootStatus::Processing)?;
        }
    }
    queued.shoots = shoots;
    Ok(queued)
}

/// Queues analysis for anything in the shoot that is not finished.
pub fn queue_pending_work(db: &Database, shoot_id: i64) -> Result<usize> {
    let pending = {
        let mut conn = db.conn()?;
        media_repo::pending(&mut conn, shoot_id, i64::MAX)?
    };

    let queued = db.transaction(|conn| {
        let mut count = 0;
        for item in &pending {
            if item.processing_status == ProcessingStatus::Pending.as_str() || item.thumbnail_path.is_none() {
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
        let mut conn = db.conn()?;
        queue_finishing_stages(&mut conn, shoot_id)?;
        shoots::set_status(&mut conn, shoot_id, ShootStatus::Processing)?;
    }

    Ok(queued)
}

#[cfg(test)]
mod tests {
    use super::*;
    use skwad_database::models::BoundingBox;
    use skwad_database::repo::people;

    fn seed_shoot(db: &Database) -> i64 {
        let mut conn = db.conn().unwrap();
        shoots::create(&mut conn, "Test Shoot", "C:\\shoot").unwrap().id
    }

    fn add_face(db: &Database, shoot_id: i64, filename: &str, embedding: Vec<f32>) -> (i64, i64) {
        add_face_in_cohort(db, shoot_id, filename, embedding, None)
    }

    fn add_face_in_cohort(
        db: &Database,
        shoot_id: i64,
        filename: &str,
        embedding: Vec<f32>,
        model_key: Option<&str>,
    ) -> (i64, i64) {
        let mut conn = db.conn().unwrap();
        let media_id = media_repo::upsert(
            &mut conn,
            &NewMedia {
                shoot_id,
                path: format!("C:\\shoot\\{filename}"),
                filename: filename.to_string(),
                media_type: MediaType::Photo,
                extension: filename.split('.').next_back().unwrap_or_default().to_ascii_lowercase(),
                file_size: 1,
                content_key: filename.to_string(),
                captured_at: None,
                normalized_relative_path: None,
            },
        )
        .unwrap();
        let face_id = faces::insert(
            &mut conn,
            &skwad_database::models::NewFace {
                media_id,
                shoot_id,
                bbox: BoundingBox {
                    x: 0.1,
                    y: 0.1,
                    w: 0.2,
                    h: 0.2,
                },
                landmarks: None,
                detection_confidence: 0.95,
                embedding: Some(embedding),
                quality: Some(0.7),
                frame_time: None,
                crop_path: None,
                model_key: model_key.map(String::from),
            },
        )
        .unwrap();
        (media_id, face_id)
    }

    fn add_video_faces(db: &Database, shoot_id: i64, filename: &str, samples: &[(f64, Vec<f32>)]) -> Vec<i64> {
        let mut conn = db.conn().unwrap();
        let media_id = media_repo::upsert(
            &mut conn,
            &NewMedia {
                shoot_id,
                path: format!("C:\\shoot\\{filename}"),
                filename: filename.to_string(),
                media_type: MediaType::Video,
                extension: "mp4".into(),
                file_size: 1,
                content_key: filename.to_string(),
                captured_at: None,
                normalized_relative_path: None,
            },
        )
        .unwrap();
        samples
            .iter()
            .map(|(frame_time, embedding)| {
                faces::insert(
                    &mut conn,
                    &skwad_database::models::NewFace {
                        media_id,
                        shoot_id,
                        bbox: BoundingBox {
                            x: 0.1,
                            y: 0.1,
                            w: 0.2,
                            h: 0.2,
                        },
                        landmarks: None,
                        detection_confidence: 0.95,
                        embedding: Some(embedding.clone()),
                        quality: Some(0.7),
                        frame_time: Some(*frame_time),
                        crop_path: None,
                        model_key: None,
                    },
                )
                .unwrap()
            })
            .collect()
    }

    fn unit(v: Vec<f32>) -> Vec<f32> {
        let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        v.into_iter().map(|x| x / norm).collect()
    }

    #[test]
    fn recognition_suggests_rather_than_confirms_by_default() {
        let db = Database::open_test().unwrap();
        let shoot_id = seed_shoot(&db);

        // A known player, established by a confirmed face in an earlier shoot.
        let (_, known_face) = add_face(&db, shoot_id, "known.jpg", unit(vec![1.0, 0.0, 0.0]));
        {
            let mut conn = db.conn().unwrap();
            let person = people::get_or_create(&mut conn, "Jonathan", None).unwrap();
            faces::assign(&mut conn, known_face, person.id, Some(1.0)).unwrap();
        }

        // RAW and finished stills share the exact same recognition route once
        // their pixels have been decoded into the common photo representation.
        let (_, new_face) = add_face(&db, shoot_id, "new.RAF", unit(vec![0.97, 0.1, 0.0]));

        let report = recognise_shoot(&db, shoot_id, &AppSettings::default()).unwrap();
        assert_eq!(report.library_players, 1);
        assert_eq!(report.faces_matched, 1);
        assert_eq!(report.faces_auto_confirmed, 0, "nothing is auto-confirmed by default");

        let mut conn = db.conn().unwrap();
        let face = faces::get_by_id(&mut conn, new_face).unwrap().unwrap();
        assert_eq!(face.assignment, "suggested");
        assert!(face.person_id.is_some());
    }

    #[test]
    fn naming_a_raw_reference_after_the_initial_pass_enables_matching() {
        let db = Database::open_test().unwrap();
        let shoot_id = seed_shoot(&db);
        let (_, reference) = add_face(&db, shoot_id, "reference.RAF", unit(vec![1.0, 0.0, 0.0]));
        let (_, similar) = add_face(&db, shoot_id, "similar.RAF", unit(vec![0.98, 0.08, 0.0]));

        let first_pass = recognise_shoot(&db, shoot_id, &AppSettings::default()).unwrap();
        assert_eq!(first_pass.library_players, 0);
        assert_eq!(first_pass.faces_matched, 0);

        {
            let mut conn = db.conn().unwrap();
            let person = people::get_or_create(&mut conn, "Jonathan", None).unwrap();
            faces::assign(&mut conn, reference, person.id, None).unwrap();
        }

        let after_naming = recognise_shoot(&db, shoot_id, &AppSettings::default()).unwrap();
        assert_eq!(after_naming.faces_matched, 1);
        let mut conn = db.conn().unwrap();
        let matched = faces::get_by_id(&mut conn, similar).unwrap().unwrap();
        assert_eq!(matched.assignment, "suggested");
        assert!(matched.person_id.is_some());
    }

    #[test]
    fn the_same_player_can_match_at_multiple_video_sample_times() {
        let db = Database::open_test().unwrap();
        let shoot_id = seed_shoot(&db);
        let (_, reference) = add_face(&db, shoot_id, "reference.jpg", unit(vec![1.0, 0.0, 0.0]));
        {
            let mut conn = db.conn().unwrap();
            let person = people::get_or_create(&mut conn, "Jonathan", None).unwrap();
            faces::assign(&mut conn, reference, person.id, Some(1.0)).unwrap();
        }
        let samples = add_video_faces(
            &db,
            shoot_id,
            "interview.mp4",
            &[(0.0, unit(vec![0.99, 0.02, 0.0])), (5.0, unit(vec![0.98, 0.05, 0.0]))],
        );

        let report = recognise_shoot(&db, shoot_id, &AppSettings::default()).unwrap();
        assert_eq!(report.faces_matched, 2);
        let mut conn = db.conn().unwrap();
        assert!(samples.into_iter().all(|face_id| {
            let face = faces::get_by_id(&mut conn, face_id).unwrap().unwrap();
            face.person_id.is_some() && face.assignment == "suggested"
        }));
    }

    #[test]
    fn auto_confirm_applies_above_its_threshold() {
        let db = Database::open_test().unwrap();
        let shoot_id = seed_shoot(&db);
        let (_, known_face) = add_face(&db, shoot_id, "known.jpg", unit(vec![1.0, 0.0, 0.0]));
        {
            let mut conn = db.conn().unwrap();
            let person = people::get_or_create(&mut conn, "Jonathan", None).unwrap();
            faces::assign(&mut conn, known_face, person.id, Some(1.0)).unwrap();
        }
        let (_, new_face) = add_face(&db, shoot_id, "new.jpg", unit(vec![0.999, 0.02, 0.0]));

        let settings = AppSettings {
            auto_confirm_above: 0.9,
            ..Default::default()
        };
        let report = recognise_shoot(&db, shoot_id, &settings).unwrap();
        assert_eq!(report.faces_auto_confirmed, 1);

        let mut conn = db.conn().unwrap();
        assert_eq!(
            faces::get_by_id(&mut conn, new_face).unwrap().unwrap().assignment,
            "confirmed"
        );
    }

    #[test]
    fn a_stricter_rerun_removes_a_stale_suggestion() {
        let db = Database::open_test().unwrap();
        let shoot_id = seed_shoot(&db);
        let (_, known_face) = add_face(&db, shoot_id, "known.jpg", unit(vec![1.0, 0.0]));
        let (_, borderline) = add_face(&db, shoot_id, "borderline.jpg", unit(vec![0.6, 0.8]));
        {
            let mut conn = db.conn().unwrap();
            let person = people::get_or_create(&mut conn, "Jonathan", None).unwrap();
            faces::assign(&mut conn, known_face, person.id, Some(1.0)).unwrap();
        }

        let permissive = AppSettings {
            recognition_threshold: 0.5,
            recognition_margin: 0.0,
            ..Default::default()
        };
        assert_eq!(recognise_shoot(&db, shoot_id, &permissive).unwrap().faces_matched, 1);

        let strict = AppSettings {
            recognition_threshold: 0.8,
            recognition_margin: 0.0,
            ..Default::default()
        };
        assert_eq!(recognise_shoot(&db, shoot_id, &strict).unwrap().faces_matched, 0);
        let mut conn = db.conn().unwrap();
        let face = faces::get_by_id(&mut conn, borderline).unwrap().unwrap();
        assert_eq!(face.assignment, "unassigned");
        assert_eq!(face.person_id, None);
    }

    /// Two embedders' vectors live in different spaces. Identical numbers from
    /// different cohorts must neither match nor cluster together, however
    /// similar they look.
    #[test]
    fn vectors_from_different_embedders_are_never_compared() {
        let db = Database::open_test().unwrap();
        let shoot_id = seed_shoot(&db);

        let (_, known) = add_face_in_cohort(&db, shoot_id, "known.jpg", unit(vec![1.0, 0.0, 0.0]), Some("model-a"));
        {
            let mut conn = db.conn().unwrap();
            let person = people::get_or_create(&mut conn, "Jonathan", None).unwrap();
            faces::assign(&mut conn, known, person.id, Some(1.0)).unwrap();
        }
        let (_, same_cohort) =
            add_face_in_cohort(&db, shoot_id, "a.jpg", unit(vec![0.99, 0.05, 0.0]), Some("model-a"));
        let (_, other_cohort) =
            add_face_in_cohort(&db, shoot_id, "b.jpg", unit(vec![0.99, 0.05, 0.0]), Some("model-b"));
        let (_, legacy) = add_face(&db, shoot_id, "c.jpg", unit(vec![0.99, 0.05, 0.0]));

        let report = recognise_shoot(&db, shoot_id, &AppSettings::default()).unwrap();
        assert_eq!(report.faces_matched, 1, "only the same-cohort face can match");
        let mut conn = db.conn().unwrap();
        assert_eq!(faces::get_by_id(&mut conn, same_cohort).unwrap().unwrap().assignment, "suggested");
        assert_eq!(faces::get_by_id(&mut conn, other_cohort).unwrap().unwrap().assignment, "unassigned");
        assert_eq!(faces::get_by_id(&mut conn, legacy).unwrap().unwrap().assignment, "unassigned");
        drop(conn);

        // Clustering: five near-identical vectors in each of two cohorts make
        // two clusters, never one.
        let shoot_b = seed_shoot(&db);
        for i in 0..5 {
            add_face_in_cohort(&db, shoot_b, &format!("a{i}.jpg"), unit(vec![1.0, 0.02 * i as f32, 0.0]), Some("model-a"));
            add_face_in_cohort(&db, shoot_b, &format!("b{i}.jpg"), unit(vec![1.0, 0.02 * i as f32, 0.0]), Some("model-b"));
        }
        let report = cluster_shoot(&db, shoot_b, &AppSettings::default()).unwrap();
        assert_eq!(report.clusters_created, 2);
        assert_eq!(report.faces_clustered, 10);
        let mut conn = db.conn().unwrap();
        let labels: Vec<String> = clusters::list_summaries(&mut conn, shoot_b, false)
            .unwrap()
            .into_iter()
            .map(|s| s.cluster.label)
            .collect();
        assert_eq!(labels, vec!["Unknown Person 1", "Unknown Person 2"]);

        // The stale-cohort report and the re-embed queue see exactly the
        // faces that are not in the current cohort.
        let stale = faces::stale_embeddings(&mut conn, "model-a", None).unwrap();
        assert_eq!(stale.faces, 1 + 1 + 5, "model-b faces plus the legacy NULL one");
        assert_eq!(stale.media, 7);
        drop(conn);
        let queued = queue_reembed(&db, "model-a", None).unwrap();
        assert_eq!(queued.media, 7);
        assert_eq!(queued.shoots, vec![shoot_id, shoot_b]);
    }

    #[test]
    fn reference_selection_keeps_the_best_and_caps_good_extras() {
        let samples = (0..12)
            .map(|face_id| skwad_database::repo::faces::FaceVector {
                face_id,
                media_id: face_id,
                frame_time: None,
                person_id: Some(7),
                embedding: vec![1.0, 0.0],
                quality: if face_id == 11 { 0.95 } else { 0.7 },
                model_key: None,
            })
            .chain(std::iter::once(skwad_database::repo::faces::FaceVector {
                face_id: 99,
                media_id: 99,
                frame_time: None,
                person_id: Some(8),
                embedding: vec![0.0, 1.0],
                quality: 0.2,
                model_key: None,
            }))
            .collect();

        let selected = select_reference_vectors(samples);
        assert_eq!(selected.iter().filter(|v| v.person_id == Some(7)).count(), 8);
        assert_eq!(selected.iter().filter(|v| v.person_id == Some(8)).count(), 1);
        assert!(selected.iter().any(|v| v.face_id == 11));
    }

    #[test]
    fn an_empty_library_leaves_everything_for_clustering() {
        let db = Database::open_test().unwrap();
        let shoot_id = seed_shoot(&db);
        add_face(&db, shoot_id, "a.jpg", unit(vec![1.0, 0.0, 0.0]));

        let report = recognise_shoot(&db, shoot_id, &AppSettings::default()).unwrap();
        assert_eq!(report.library_players, 0);
        assert_eq!(report.faces_matched, 0);
    }

    #[test]
    fn match_person_in_shoot_suggests_without_touching_other_people() {
        let db = Database::open_test().unwrap();
        let shoot_id = seed_shoot(&db);
        let (_, existing_known) = add_face(&db, shoot_id, "known.jpg", unit(vec![0.0, 1.0, 0.0]));
        let jonathan = {
            let mut conn = db.conn().unwrap();
            let p = people::get_or_create(&mut conn, "Jonathan", None).unwrap();
            faces::assign(&mut conn, existing_known, p.id, Some(1.0)).unwrap();
            p
        };

        // A person enrolled after this shoot was already processed — their
        // reference sample lives outside the shoot, like the hidden
        // Reference Library shoot enrollment uses.
        let reference_shoot_id = {
            let mut conn = db.conn().unwrap();
            shoots::create(&mut conn, "Reference Library", "").unwrap().id
        };
        let mavi = {
            let mut conn = db.conn().unwrap();
            people::get_or_create(&mut conn, "Mavi", None).unwrap()
        };
        let (_, reference_face) = add_face(&db, reference_shoot_id, "ref.jpg", unit(vec![1.0, 0.0, 0.0]));
        {
            let mut conn = db.conn().unwrap();
            faces::assign(&mut conn, reference_face, mavi.id, Some(1.0)).unwrap();
        }

        let (_, candidate) = add_face(&db, shoot_id, "candidate.jpg", unit(vec![0.99, 0.05, 0.0]));

        let new_suggestions = match_person_in_shoot(&db, shoot_id, mavi.id, &AppSettings::default()).unwrap();
        assert_eq!(new_suggestions, 1);

        let mut conn = db.conn().unwrap();
        let matched = faces::get_by_id(&mut conn, candidate).unwrap().unwrap();
        assert_eq!(matched.person_id, Some(mavi.id));
        assert_eq!(matched.assignment, "suggested");

        // Jonathan's confirmed face in the same shoot is untouched.
        let unaffected = faces::get_by_id(&mut conn, existing_known).unwrap().unwrap();
        assert_eq!(unaffected.person_id, Some(jonathan.id));
        assert_eq!(unaffected.assignment, "confirmed");
        drop(conn);

        // Safe to re-run: the face is no longer "unassigned" (it already
        // carries a suggestion), so nothing new is proposed.
        assert_eq!(
            match_person_in_shoot(&db, shoot_id, mavi.id, &AppSettings::default()).unwrap(),
            0
        );
    }

    #[test]
    fn clustering_groups_unknown_faces_and_names_them_in_order() {
        let db = Database::open_test().unwrap();
        let shoot_id = seed_shoot(&db);

        for i in 0..5 {
            add_face(
                &db,
                shoot_id,
                &format!("a{i}.jpg"),
                unit(vec![1.0, 0.02 * i as f32, 0.0]),
            );
        }
        for i in 0..3 {
            add_face(
                &db,
                shoot_id,
                &format!("b{i}.jpg"),
                unit(vec![0.0, 0.02 * i as f32, 1.0]),
            );
        }

        let report = cluster_shoot(&db, shoot_id, &AppSettings::default()).unwrap();
        assert_eq!(report.clusters_created, 2);
        assert_eq!(report.faces_clustered, 8);

        let mut conn = db.conn().unwrap();
        let summaries = clusters::list_summaries(&mut conn, shoot_id, false).unwrap();
        assert_eq!(summaries[0].cluster.label, "Unknown Person 1");
        assert_eq!(summaries[0].cluster.face_count, 5);
        assert_eq!(summaries[1].cluster.face_count, 3);
    }

    #[test]
    fn reclustering_preserves_a_named_cluster() {
        let db = Database::open_test().unwrap();
        let shoot_id = seed_shoot(&db);
        for i in 0..5 {
            add_face(
                &db,
                shoot_id,
                &format!("a{i}.jpg"),
                unit(vec![1.0, 0.02 * i as f32, 0.0]),
            );
        }

        cluster_shoot(&db, shoot_id, &AppSettings::default()).unwrap();

        let named_id = {
            let mut conn = db.conn().unwrap();
            let summaries = clusters::list_summaries(&mut conn, shoot_id, false).unwrap();
            let person = people::get_or_create(&mut conn, "Jelly", None).unwrap();
            clusters::name_cluster(&mut conn, summaries[0].cluster.id, person.id).unwrap();
            summaries[0].cluster.id
        };

        // Re-running must not undo the identification.
        cluster_shoot(&db, shoot_id, &AppSettings::default()).unwrap();

        let mut conn = db.conn().unwrap();
        let cluster = clusters::get_by_id(&mut conn, named_id)
            .unwrap()
            .expect("named cluster survives");
        assert_eq!(cluster.status, "named");
        assert_eq!(faces::library_vectors(&mut conn).unwrap().len(), 1);
    }

    #[test]
    fn albums_are_generated_and_the_shoot_completes() {
        let db = Database::open_test().unwrap();
        let shoot_id = seed_shoot(&db);
        let (_, face_id) = add_face(&db, shoot_id, "a.jpg", unit(vec![1.0, 0.0]));
        {
            let mut conn = db.conn().unwrap();
            let person = people::get_or_create(&mut conn, "Mavi", None).unwrap();
            faces::assign(&mut conn, face_id, person.id, Some(0.99)).unwrap();
        }

        assert!(generate_albums(&db, shoot_id).unwrap() >= 1);

        let mut conn = db.conn().unwrap();
        assert_eq!(
            shoots::get_by_id(&mut conn, shoot_id).unwrap().unwrap().status,
            "completed"
        );
        assert!(albums::list(&mut conn, shoot_id)
            .unwrap()
            .iter()
            .any(|a| a.name == "Mavi"));
    }

    /// `normalized_relative_path` existed in the schema and was exported by
    /// portable catalogues, but nothing wrote it. The scanner is the only
    /// place that knows both the root and the file, so it fills it in.
    #[test]
    fn scanning_records_the_path_relative_to_the_shoot_root() {
        let source = tempfile::tempdir().unwrap();
        let day = source.path().join("day1");
        std::fs::create_dir_all(&day).unwrap();
        let mut image = image::RgbImage::new(16, 16);
        for pixel in image.pixels_mut() {
            *pixel = image::Rgb([10, 20, 30]);
        }
        image.save(day.join("IMG_0001.jpg")).unwrap();
        image.save(source.path().join("IMG_0002.jpg")).unwrap();

        let db = Database::open_test().unwrap();
        let shoot_id = {
            let mut conn = db.conn().unwrap();
            shoots::create(&mut conn, "Finals", &source.path().display().to_string())
                .unwrap()
                .id
        };

        let summary = scan_shoot(&db, shoot_id, &AppSettings::default(), None, |_| {}).unwrap();
        assert_eq!(summary.photos, 2);

        let mut conn = db.conn().unwrap();
        let mut relative: Vec<Option<String>> = media_repo::query(
            &mut conn,
            &skwad_database::models::MediaQuery {
                shoot_id: Some(shoot_id),
                ..Default::default()
            },
        )
        .unwrap()
        .into_iter()
        .map(|m| m.normalized_relative_path)
        .collect();
        relative.sort();
        assert_eq!(
            relative,
            vec![Some("IMG_0002.jpg".to_string()), Some("day1/IMG_0001.jpg".to_string())]
        );
    }

    #[test]
    fn reset_clears_derived_data_but_keeps_the_media_index() {
        let db = Database::open_test().unwrap();
        let shoot_id = seed_shoot(&db);
        add_face(&db, shoot_id, "a.jpg", unit(vec![1.0, 0.0]));
        cluster_shoot(&db, shoot_id, &AppSettings::default()).unwrap();
        generate_albums(&db, shoot_id).unwrap();

        reset_analysis(&db, shoot_id).unwrap();

        let mut conn = db.conn().unwrap();
        assert_eq!(
            media_repo::count_for_shoot(&mut conn, shoot_id).unwrap(),
            1,
            "the file index survives"
        );
        assert!(faces::for_media(&mut conn, 1).unwrap().is_empty());
        assert!(albums::list(&mut conn, shoot_id).unwrap().is_empty());
        assert!(clusters::list_summaries(&mut conn, shoot_id, true).unwrap().is_empty());
    }
}
