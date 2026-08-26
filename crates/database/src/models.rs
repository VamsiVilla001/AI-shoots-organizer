//! Row types and the enums that back the `TEXT` status columns.
//!
//! Everything here serialises to camelCase so it can cross the Tauri IPC
//! boundary and land in the TypeScript layer unchanged. The mirrored TS
//! definitions live in `packages/shared-types`.

use serde::{Deserialize, Serialize};

macro_rules! string_enum {
    ($name:ident { $($(#[$meta:meta])* $variant:ident => $text:literal),+ $(,)? }) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(rename_all = "camelCase")]
        pub enum $name {
            $($(#[$meta])* $variant),+
        }

        impl $name {
            pub fn as_str(&self) -> &'static str {
                match self { $(Self::$variant => $text),+ }
            }

            pub fn parse(s: &str) -> Option<Self> {
                match s { $($text => Some(Self::$variant),)+ _ => None }
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl rusqlite::ToSql for $name {
            fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
                Ok(rusqlite::types::ToSqlOutput::from(self.as_str()))
            }
        }
    };
}

string_enum!(ShootStatus {
    Created => "created",
    Scanning => "scanning",
    Scanned => "scanned",
    Processing => "processing",
    Paused => "paused",
    Completed => "completed",
    Failed => "failed",
});

string_enum!(MediaType {
    Photo => "photo",
    Video => "video",
});

string_enum!(ProcessingStatus {
    Pending => "pending",
    Indexed => "indexed",
    Thumbnailed => "thumbnailed",
    Analysing => "analysing",
    Analysed => "analysed",
    Failed => "failed",
    Skipped => "skipped",
});

string_enum!(FaceAssignment {
    /// Detected, but not matched to anyone yet.
    Unassigned => "unassigned",
    /// The matcher proposed a person; a human has not agreed yet.
    Suggested => "suggested",
    /// A human confirmed this face belongs to `person_id`.
    Confirmed => "confirmed",
    /// A human said the suggestion was wrong.
    Rejected => "rejected",
    /// Not a usable face (false positive, crowd background, motion blur).
    Ignored => "ignored",
});

string_enum!(ClusterStatus {
    Unnamed => "unnamed",
    Named => "named",
    Ignored => "ignored",
});

string_enum!(AlbumType {
    Player => "player",
    MultiPlayer => "multiPlayer",
    Unidentified => "unidentified",
    Team => "team",
    /// Grouped by how many people are in the file, independently of who they
    /// are: "Single", "Two persons", and so on.
    GroupSize => "groupSize",
});

string_enum!(JobKind {
    Scan => "scan",
    Thumbnail => "thumbnail",
    AnalysePhoto => "analysePhoto",
    AnalyseVideo => "analyseVideo",
    Recognise => "recognise",
    Cluster => "cluster",
    Albums => "albums",
});

string_enum!(JobState {
    Queued => "queued",
    Running => "running",
    Done => "done",
    Failed => "failed",
    Cancelled => "cancelled",
});

string_enum!(ExportStatus {
    Queued => "queued",
    Running => "running",
    Completed => "completed",
    Failed => "failed",
    Cancelled => "cancelled",
});

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Shoot {
    pub id: i64,
    pub name: String,
    pub source_path: String,
    pub status: String,
    pub notes: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// A shoot plus the counts the Shoots screen renders (§21).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShootSummary {
    #[serde(flatten)]
    pub shoot: Shoot,
    pub photo_count: i64,
    pub video_count: i64,
    pub face_count: i64,
    pub person_count: i64,
    pub unknown_cluster_count: i64,
    pub pending_jobs: i64,
    pub failed_jobs: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Media {
    pub id: i64,
    pub shoot_id: i64,
    pub path: String,
    pub filename: String,
    pub media_type: String,
    pub extension: String,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub duration: Option<f64>,
    pub file_size: i64,
    pub content_key: String,
    pub captured_at: Option<String>,
    pub indexed_at: String,
    pub camera_make: Option<String>,
    pub camera_model: Option<String>,
    pub lens: Option<String>,
    pub iso: Option<i64>,
    pub focal_length: Option<f64>,
    pub aperture: Option<f64>,
    pub shutter: Option<String>,
    pub orientation: i64,
    pub thumbnail_path: Option<String>,
    pub processing_status: String,
    /// Number of detected face *rows*. For a video this counts every sampled
    /// frame, so it is not the number of people — see `person_count`.
    pub face_count: i64,
    /// Number of distinct people in the file, which is what group-size albums
    /// are built from.
    pub person_count: i64,
    /// Editorial quality hints derived locally from the cached thumbnail.
    pub quality_score: Option<f64>,
    pub sharpness_score: Option<f64>,
    pub exposure_score: Option<f64>,
    /// Hex-encoded 64-bit difference hash used for near-duplicate grouping.
    pub perceptual_hash: Option<String>,
    pub duplicate_group_id: Option<i64>,
    pub duplicate_count: i64,
    pub is_best_shot: bool,
    pub error: Option<String>,
}

/// A newly scanned file, before it has an id.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewMedia {
    pub shoot_id: i64,
    pub path: String,
    pub filename: String,
    pub media_type: MediaType,
    pub extension: String,
    pub file_size: i64,
    pub content_key: String,
    pub captured_at: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaMetadata {
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub duration: Option<f64>,
    pub captured_at: Option<String>,
    pub camera_make: Option<String>,
    pub camera_model: Option<String>,
    pub lens: Option<String>,
    pub iso: Option<i64>,
    pub focal_length: Option<f64>,
    pub aperture: Option<f64>,
    pub shutter: Option<String>,
    pub orientation: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Person {
    pub id: i64,
    pub name: String,
    pub team: Option<String>,
    pub notes: Option<String>,
    pub cover_face_id: Option<i64>,
    pub created_at: String,
    pub updated_at: String,
}

/// A player plus the library statistics the Players screen shows (§22).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PersonSummary {
    #[serde(flatten)]
    pub person: Person,
    pub face_sample_count: i64,
    pub media_count: i64,
    pub shoot_count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Face {
    pub id: i64,
    pub media_id: i64,
    pub shoot_id: i64,
    pub person_id: Option<i64>,
    pub cluster_id: Option<i64>,
    pub embedding_dim: Option<i64>,
    pub bbox: BoundingBox,
    pub detection_confidence: f64,
    pub recognition_confidence: Option<f64>,
    pub assignment: String,
    pub quality: Option<f64>,
    pub frame_time: Option<f64>,
    pub crop_path: Option<String>,
    pub created_at: String,
}

/// Normalised against the full frame, so it survives thumbnail resizing.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoundingBox {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// A face row joined with what the review screen needs to draw it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FaceWithContext {
    #[serde(flatten)]
    pub face: Face,
    pub media_path: String,
    pub media_filename: String,
    pub media_type: String,
    pub thumbnail_path: Option<String>,
    pub person_name: Option<String>,
    pub cluster_label: Option<String>,
}

#[derive(Debug, Clone)]
pub struct NewFace {
    pub media_id: i64,
    pub shoot_id: i64,
    pub bbox: BoundingBox,
    pub landmarks: Option<Vec<f32>>,
    pub detection_confidence: f64,
    pub embedding: Option<Vec<f32>>,
    pub quality: Option<f64>,
    pub frame_time: Option<f64>,
    pub crop_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Cluster {
    pub id: i64,
    pub shoot_id: i64,
    pub label: String,
    pub person_id: Option<i64>,
    pub status: String,
    pub face_count: i64,
    pub cover_face_id: Option<i64>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClusterSummary {
    #[serde(flatten)]
    pub cluster: Cluster,
    pub media_count: i64,
    pub person_name: Option<String>,
    pub cover_media_id: Option<i64>,
    pub cover_thumbnail_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Album {
    pub id: i64,
    pub shoot_id: i64,
    pub name: String,
    pub album_type: String,
    pub person_ids: Vec<i64>,
    pub cluster_id: Option<i64>,
    pub cover_media_id: Option<i64>,
    pub media_count: i64,
    pub photo_count: i64,
    pub video_count: i64,
    pub sort_order: i64,
    pub generated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VideoDetection {
    pub id: i64,
    pub media_id: i64,
    pub person_id: Option<i64>,
    pub face_id: Option<i64>,
    pub timestamp: f64,
    pub end_timestamp: Option<f64>,
    pub confidence: f64,
}

/// A person's appearances in one video, collapsed into ranges for the timeline.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VideoTimeline {
    pub media_id: i64,
    pub person_id: Option<i64>,
    pub person_name: Option<String>,
    pub appearances: Vec<VideoDetection>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Job {
    pub id: i64,
    pub shoot_id: i64,
    pub media_id: Option<i64>,
    pub kind: String,
    pub state: String,
    pub priority: i64,
    pub attempts: i64,
    pub payload: Option<String>,
    pub error: Option<String>,
    pub created_at: String,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
}

/// The counters behind the progress panel in §18.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessingProgress {
    pub shoot_id: i64,
    pub media_total: i64,
    pub media_scanned: i64,
    pub media_analysed: i64,
    pub media_failed: i64,
    pub faces_detected: i64,
    pub faces_recognised: i64,
    pub faces_unknown: i64,
    pub jobs_queued: i64,
    pub jobs_running: i64,
    pub jobs_failed: i64,
    pub percent: f64,
    pub stage: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportRecord {
    pub id: i64,
    pub shoot_id: i64,
    pub destination: String,
    pub options: String,
    pub status: String,
    pub files_total: i64,
    pub files_done: i64,
    pub bytes_done: i64,
    pub error: Option<String>,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LogEntry {
    pub id: i64,
    pub timestamp: String,
    pub event: String,
    pub shoot_id: Option<i64>,
    pub media_id: Option<i64>,
    pub person_id: Option<i64>,
    pub detail: Option<String>,
}

/// Filters for the media grid.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct MediaQuery {
    pub shoot_id: Option<i64>,
    pub person_id: Option<i64>,
    pub cluster_id: Option<i64>,
    pub album_id: Option<i64>,
    pub media_type: Option<String>,
    pub search: Option<String>,
    /// Only media with at least one face nobody has identified yet.
    pub only_unidentified: bool,
    /// Only media holding exactly this many people. Values at or above the
    /// group-size cap mean "this many or more", matching how the albums bucket.
    pub group_size: Option<i64>,
    /// Keep one highest-quality photo from every duplicate group, plus unique
    /// photos. Videos are excluded when this filter is active.
    pub only_best_shots: bool,
    /// Show only photos that belong to a near-duplicate group.
    pub only_duplicates: bool,
    /// capturedAt (default) | quality | filename.
    pub sort: Option<String>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

/// Filters for the review workspace.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct FaceQuery {
    pub shoot_id: Option<i64>,
    pub person_id: Option<i64>,
    pub cluster_id: Option<i64>,
    pub assignment: Option<String>,
    pub min_confidence: Option<f64>,
    pub max_confidence: Option<f64>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}
