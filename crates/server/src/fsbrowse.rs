//! `/api/fs/*` — the client's replacement for a native folder picker.
//!
//! A shoot's source path has to be meaningful to the *scanner*, which runs on
//! this machine, so the picker browses this machine's folders — its drives
//! and any share it can reach, the way a file explorer would. When
//! `SKWAD_SERVER_MEDIA_ROOTS` is set, browsing is confined to those folders:
//! every path is canonicalised and confirmed to sit inside one before
//! anything is read, so `..`, symlinks and absolute paths elsewhere fail
//! closed. Listing a directory returns its subdirectories and a count of
//! media files — what the picker needs to show and nothing more.

use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::{Query, State};
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::config::{resolve_within_roots, without_verbatim_prefix};
use crate::error::{blocking, ApiError, ApiResult};
use crate::state::ServerState;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FsRoot {
    /// The path as configured, which is what a caller passes back to `list`.
    pub path: String,
    /// The last component, for display.
    pub name: String,
    pub available: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FsEntry {
    pub path: String,
    pub name: String,
    /// Media files directly inside this directory, not counting subdirectories.
    pub media_count: usize,
    pub has_subfolders: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FsListing {
    pub path: String,
    /// `None` at a root — there is nowhere further up to go.
    pub parent: Option<String>,
    pub directories: Vec<FsEntry>,
    pub media_count: usize,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListQuery {
    pub path: String,
}

/// The places a picker starts from: the configured media roots, or — when
/// none are configured — every drive this machine has. A share is reached by
/// typing its UNC path into the folder field.
pub async fn roots(State(state): State<Arc<ServerState>>) -> ApiResult<Json<Vec<FsRoot>>> {
    let media_roots = state.config.media_roots.clone();
    let listing = blocking(move || {
        let roots = if media_roots.is_empty() { machine_drives() } else { media_roots };
        Ok(roots
            .iter()
            .map(|root| FsRoot {
                path: root.display().to_string(),
                name: root
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .filter(|n| !n.is_empty())
                    .unwrap_or_else(|| root.display().to_string()),
                available: root.is_dir(),
            })
            .collect())
    })
    .await?;
    Ok(Json(listing))
}

/// The drives present on this machine, as roots.
fn machine_drives() -> Vec<PathBuf> {
    if cfg!(windows) {
        (b'A'..=b'Z')
            .map(|letter| PathBuf::from(format!("{}:\\", letter as char)))
            .filter(|drive| drive.is_dir())
            .collect()
    } else {
        vec![PathBuf::from("/")]
    }
}

/// Where browsing may go. With media roots configured, inside them only;
/// without, anywhere this machine can read — the picker is then the file
/// explorer the person expected, for the machine that will do the scanning.
fn resolve_browse_path(requested: &PathBuf, roots: &[PathBuf]) -> ApiResult<PathBuf> {
    if roots.is_empty() {
        let canonical = std::fs::canonicalize(requested)
            .map_err(|e| ApiError::not_found(format!("{}: {e}", requested.display())))?;
        return Ok(without_verbatim_prefix(canonical));
    }
    Ok(resolve_within_roots(requested, roots)?)
}

pub async fn list(
    State(state): State<Arc<ServerState>>,
    Query(query): Query<ListQuery>,
) -> ApiResult<Json<FsListing>> {
    let roots = state.config.media_roots.clone();
    let requested = PathBuf::from(&query.path);
    let listing = blocking(move || {
        let canonical = resolve_browse_path(&requested, &roots)?;

        let mut directories = Vec::new();
        let mut media_count = 0usize;

        let entries = std::fs::read_dir(&canonical)
            .map_err(|e| ApiError::not_found(format!("{}: {e}", canonical.display())))?;

        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(file_type) = entry.file_type() else { continue };

            if file_type.is_dir() {
                let (child_media, child_dirs) = shallow_counts(&path);
                directories.push(FsEntry {
                    path: path.display().to_string(),
                    name: entry.file_name().to_string_lossy().to_string(),
                    media_count: child_media,
                    has_subfolders: child_dirs,
                });
            } else if is_media(&path) {
                media_count += 1;
            }
        }

        directories.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));

        // A parent is only offered while it is still inside a root, so the
        // picker cannot walk out of the jail one level at a time. Without
        // roots the only stop is the top of the drive.
        let parent = canonical
            .parent()
            .filter(|p| resolve_browse_path(&p.to_path_buf(), &roots).is_ok())
            .map(|p| p.display().to_string());

        Ok(FsListing {
            path: canonical.display().to_string(),
            parent,
            directories,
            media_count,
        })
    })
    .await?;

    Ok(Json(listing))
}

/// Counts media directly inside `dir` and notes whether it has subdirectories.
/// One level only: recursing here would walk an entire share to render a list.
fn shallow_counts(dir: &std::path::Path) -> (usize, bool) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return (0, false);
    };
    let mut media = 0usize;
    let mut has_dirs = false;
    for entry in entries.flatten() {
        match entry.file_type() {
            Ok(t) if t.is_dir() => has_dirs = true,
            Ok(_) if is_media(&entry.path()) => media += 1,
            _ => {}
        }
    }
    (media, has_dirs)
}

fn is_media(path: &std::path::Path) -> bool {
    // The scanner's own predicate, so the count the picker shows is the count
    // the shoot will actually index.
    skwad_media_core::formats::classify(path).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_detection_follows_the_scanner() {
        assert!(is_media(std::path::Path::new("a/b/IMG_0001.JPG")));
        assert!(is_media(std::path::Path::new("a/b/clip.mp4")));
        assert!(!is_media(std::path::Path::new("a/b/notes.txt")));
    }

    #[test]
    fn shallow_counts_do_not_recurse() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("one.jpg"), b"x").unwrap();
        std::fs::write(dir.path().join("two.mp4"), b"x").unwrap();
        std::fs::write(dir.path().join("notes.txt"), b"x").unwrap();
        std::fs::create_dir(dir.path().join("nested")).unwrap();
        std::fs::write(dir.path().join("nested/three.jpg"), b"x").unwrap();

        let (media, has_dirs) = shallow_counts(dir.path());
        assert_eq!(media, 2, "the nested photo is not counted here");
        assert!(has_dirs);
    }
}
