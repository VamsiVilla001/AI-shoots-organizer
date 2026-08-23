//! `/api/fs/*` — the browser's replacement for the native folder picker.
//!
//! Every path that arrives here is canonicalised and confirmed to sit inside a
//! configured root before anything is read, so `..`, symlinks and absolute
//! paths all fail closed. Listing a directory returns its subdirectories and a
//! count of media files, which is what the picker needs to show and nothing
//! more — no file names, no sizes, no way to enumerate a whole share.

use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::{Query, State};
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::config::{resolve_within_roots, JailError};
use crate::error::{blocking, ApiError, ApiResult};
use crate::state::ServerState;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FsRoot {
    /// The path as configured, which is what a caller passes back to `list`.
    pub path: String,
    /// The last component, for display.
    pub name: String,
    pub writable: bool,
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

impl From<JailError> for ApiError {
    fn from(e: JailError) -> Self {
        match e {
            JailError::NoRoots => ApiError::bad_request(e.to_string()),
            JailError::Unreadable(_) => ApiError::not_found(e.to_string()),
            // Deliberately the same message whether the path exists or not: a
            // 403 that distinguishes them is a probe for what is on the disk.
            JailError::Outside(_) => ApiError::forbidden(e.to_string()),
        }
    }
}

pub async fn roots(State(state): State<Arc<ServerState>>) -> ApiResult<Json<Vec<FsRoot>>> {
    let media_roots = state.config.media_roots.clone();
    let writable = state.config.writable_roots().to_vec();

    let listing = blocking(move || {
        let mut out = Vec::new();
        for root in media_roots.iter().chain(writable.iter().filter(|w| !media_roots.contains(w))) {
            out.push(FsRoot {
                path: root.display().to_string(),
                name: root
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| root.display().to_string()),
                writable: writable.contains(root),
                available: root.is_dir(),
            });
        }
        Ok(out)
    })
    .await?;

    Ok(Json(listing))
}

pub async fn list(
    State(state): State<Arc<ServerState>>,
    Query(query): Query<ListQuery>,
) -> ApiResult<Json<FsListing>> {
    // Browsing is allowed anywhere either kind of root reaches: a destination
    // has to be pickable too.
    let mut roots = state.config.media_roots.clone();
    for writable in state.config.writable_roots() {
        if !roots.contains(writable) {
            roots.push(writable.clone());
        }
    }

    let requested = PathBuf::from(&query.path);
    let listing = blocking(move || {
        let canonical = resolve_within_roots(&requested, &roots)?;

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
        // picker cannot walk out of the jail one level at a time.
        let parent = canonical
            .parent()
            .filter(|p| resolve_within_roots(p, &roots).is_ok())
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
    teo_media_core::formats::is_supported(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_detection_follows_the_scanner() {
        assert!(is_media(std::path::Path::new("a/b/IMG_0001.JPG")));
        assert!(is_media(std::path::Path::new("a/b/clip.mp4")));
        assert!(!is_media(std::path::Path::new("a/b/notes.txt")));
        assert!(!is_media(std::path::Path::new("a/b/no-extension")));
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

    #[test]
    fn an_unreadable_directory_counts_as_empty_rather_than_failing() {
        let (media, has_dirs) = shallow_counts(std::path::Path::new("Z:\\nope\\missing"));
        assert_eq!(media, 0);
        assert!(!has_dirs);
    }

    #[test]
    fn jail_errors_map_onto_sensible_statuses() {
        let outside: ApiError = JailError::Outside("/etc".into()).into();
        assert_eq!(outside.status, axum::http::StatusCode::FORBIDDEN);

        let missing: ApiError = JailError::Unreadable("/media/gone".into()).into();
        assert_eq!(missing.status, axum::http::StatusCode::NOT_FOUND);

        let unconfigured: ApiError = JailError::NoRoots.into();
        assert_eq!(unconfigured.status, axum::http::StatusCode::BAD_REQUEST);
    }
}
