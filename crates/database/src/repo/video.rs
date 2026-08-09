//! Per-video player appearances (§9).
//!
//! Sampled frames produce a scatter of hits; [`timelines`] collapses runs of
//! nearby hits into the ranges the UI turns into clickable timestamps.

use rusqlite::{params, Connection, Row};

use super::get;
use crate::models::{VideoDetection, VideoTimeline};
use crate::Result;

/// Hits closer together than this are treated as one continuous appearance.
const APPEARANCE_GAP_SECONDS: f64 = 4.0;

fn map(row: &Row<'_>) -> rusqlite::Result<VideoDetection> {
    Ok(VideoDetection {
        id: get(row, "id")?,
        media_id: get(row, "media_id")?,
        person_id: get(row, "person_id")?,
        face_id: get(row, "face_id")?,
        timestamp: get(row, "timestamp")?,
        end_timestamp: get(row, "end_timestamp")?,
        confidence: get(row, "confidence")?,
    })
}

pub fn insert(
    conn: &Connection,
    media_id: i64,
    person_id: Option<i64>,
    face_id: Option<i64>,
    timestamp: f64,
    confidence: f64,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO video_detections (media_id, person_id, face_id, timestamp, confidence)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![media_id, person_id, face_id, timestamp, confidence],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn delete_for_media(conn: &Connection, media_id: i64) -> Result<usize> {
    Ok(conn.execute("DELETE FROM video_detections WHERE media_id = ?1", params![media_id])?)
}

pub fn for_media(conn: &Connection, media_id: i64) -> Result<Vec<VideoDetection>> {
    let mut stmt =
        conn.prepare("SELECT * FROM video_detections WHERE media_id = ?1 ORDER BY person_id, timestamp")?;
    let rows = stmt
        .query_map(params![media_id], map)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// One entry per player appearing in the video, each holding merged time ranges.
pub fn timelines(conn: &Connection, media_id: i64) -> Result<Vec<VideoTimeline>> {
    let mut stmt = conn.prepare(
        "SELECT vd.*, p.name AS person_name FROM video_detections vd
      LEFT JOIN people p ON p.id = vd.person_id
          WHERE vd.media_id = ?1
          ORDER BY vd.person_id IS NULL, p.name COLLATE NOCASE, vd.timestamp",
    )?;

    let rows = stmt
        .query_map(params![media_id], |row| {
            Ok((map(row)?, row.get::<_, Option<String>>("person_name")?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let mut out: Vec<VideoTimeline> = Vec::new();
    for (detection, person_name) in rows {
        let bucket = match out
            .iter_mut()
            .find(|t| t.person_id == detection.person_id)
        {
            Some(existing) => existing,
            None => {
                out.push(VideoTimeline {
                    media_id,
                    person_id: detection.person_id,
                    person_name,
                    appearances: Vec::new(),
                });
                out.last_mut().expect("just pushed")
            }
        };

        match bucket.appearances.last_mut() {
            // Extend the run in progress rather than starting a new marker.
            Some(last) if detection.timestamp - last.end_timestamp.unwrap_or(last.timestamp) <= APPEARANCE_GAP_SECONDS => {
                last.end_timestamp = Some(detection.timestamp);
                last.confidence = last.confidence.max(detection.confidence);
            }
            _ => bucket.appearances.push(detection),
        }
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{MediaType, NewMedia};
    use crate::repo::{media, people, shoots};
    use crate::Database;

    #[test]
    fn nearby_hits_collapse_into_one_appearance() {
        let db = Database::open_in_memory().unwrap();
        let conn = db.conn().unwrap();
        let shoot = shoots::create(&conn, "S", "C:\\s").unwrap();
        let media_id = media::upsert(
            &conn,
            &NewMedia {
                shoot_id: shoot.id,
                path: "C:\\s\\final.mp4".into(),
                filename: "final.mp4".into(),
                media_type: MediaType::Video,
                extension: "mp4".into(),
                file_size: 10,
                content_key: "k".into(),
                captured_at: None,
            },
        )
        .unwrap();
        let person = people::get_or_create(&conn, "Jonathan", None).unwrap();

        // Two clusters of hits: ~74s and ~208s.
        for t in [74.0, 76.0, 78.0, 208.0, 210.0] {
            insert(&conn, media_id, Some(person.id), None, t, 0.95).unwrap();
        }

        let timelines = timelines(&conn, media_id).unwrap();
        assert_eq!(timelines.len(), 1);
        let appearances = &timelines[0].appearances;
        assert_eq!(appearances.len(), 2);
        assert_eq!(appearances[0].timestamp, 74.0);
        assert_eq!(appearances[0].end_timestamp, Some(78.0));
        assert_eq!(appearances[1].timestamp, 208.0);
    }
}
