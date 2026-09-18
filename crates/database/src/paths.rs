//! Making stored media paths resolve on machines other than the one that
//! scanned them.
//!
//! `media.path` is absolute and is what the scanner saw. It is also an input to
//! `content_key` — `blake3(path|size|mtime)` — and the cache filename for every
//! thumbnail and proxy, so it can never be rewritten. Instead every file
//! carries two halves that *can* differ per machine:
//!
//! ```text
//! server reads   shoots.source_path  /  media.normalized_relative_path
//! client reads   shoots.share_path   /  media.normalized_relative_path
//! ```
//!
//! This module is the one place that joins them. The API layer and the export
//! engine both call [`client_path`]; nothing else should build a client-facing
//! path by hand, because a second implementation is how a shortcut ends up
//! pointing at the server's own `D:\` drive.

use std::path::{Component, Path};

use crate::models::{Media, Shoot};

/// The path of `file` below `root`, `/`-separated, or `None` when the file is
/// not under the root at all.
///
/// Comparison is component-wise on the raw strings, not through
/// `canonicalize`: the scanner already produced both paths from the same walk,
/// and canonicalising a UNC path on every file would be a network round trip.
/// The separator is folded to `/` so Windows and POSIX rows agree — this is the
/// same form the `.skwad` catalogue format has always used, which is why the
/// column was there before anything wrote it.
pub fn relative_to_root(root: &Path, file: &Path) -> Option<String> {
    let relative = file.strip_prefix(root).ok()?;
    let mut parts = Vec::new();
    for component in relative.components() {
        match component {
            Component::Normal(part) => parts.push(part.to_str()?.to_string()),
            Component::CurDir => {}
            // A relative path that climbs out of the root cannot be joined onto
            // a share, so it is better recorded as "not portable" than as
            // something that resolves to the wrong file.
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    if parts.is_empty() {
        return None;
    }
    Some(parts.join("/"))
}

/// Joins a share root and a normalised relative path in the *root's* own
/// separator convention — a UNC root keeps backslashes, a POSIX mount point
/// keeps forward slashes — so the result opens on the machine that configured
/// the root.
pub fn join_share(share_root: &str, relative: &str) -> String {
    let separator = if share_root.contains('\\') || share_root.starts_with("//") && !share_root.contains('/') {
        '\\'
    } else if share_root.contains('/') {
        '/'
    } else {
        std::path::MAIN_SEPARATOR
    };
    let trimmed = share_root.trim_end_matches(['\\', '/']);
    let relative = relative.replace('/', &separator.to_string());
    format!("{trimmed}{separator}{relative}")
}

/// The path a client machine should use to open `media`, if the shoot has a
/// share mapping and the row carries a relative path.
///
/// `None` is the honest answer for the two cases where nothing portable exists:
/// a shoot nobody has mapped to a share yet, and a row indexed before relative
/// paths were written whose absolute path did not sit under the shoot root.
/// Callers show "not reachable from this machine" rather than a path that
/// silently fails to open.
pub fn client_path(shoot: &Shoot, media: &Media) -> Option<String> {
    let share = shoot.share_path.as_deref().map(str::trim).filter(|s| !s.is_empty())?;
    let relative = media.normalized_relative_path.as_deref().filter(|s| !s.is_empty())?;
    Some(join_share(share, relative))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn shoot(share: Option<&str>) -> Shoot {
        Shoot {
            id: 1,
            name: "S".into(),
            source_path: "D:\\shoots\\finals".into(),
            share_path: share.map(String::from),
            status: "created".into(),
            notes: None,
            created_at: "now".into(),
            updated_at: "now".into(),
        }
    }

    fn media(relative: Option<&str>) -> Media {
        Media {
            id: 1,
            shoot_id: 1,
            path: "D:\\shoots\\finals\\day1\\IMG_0001.jpg".into(),
            filename: "IMG_0001.jpg".into(),
            media_type: "photo".into(),
            extension: "jpg".into(),
            width: None,
            height: None,
            duration: None,
            file_size: 1,
            content_key: "k".into(),
            normalized_relative_path: relative.map(String::from),
            captured_at: None,
            indexed_at: "now".into(),
            camera_make: None,
            camera_model: None,
            lens: None,
            iso: None,
            focal_length: None,
            aperture: None,
            shutter: None,
            orientation: 1,
            thumbnail_path: None,
            processing_status: "pending".into(),
            face_count: 0,
            person_count: 0,
            quality_score: None,
            sharpness_score: None,
            exposure_score: None,
            perceptual_hash: None,
            duplicate_group_id: None,
            duplicate_count: 1,
            is_best_shot: false,
            rating: 0,
            pick_state: "none".into(),
            error: None,
        }
    }

    #[test]
    fn relative_paths_fold_separators_and_drop_the_root() {
        let root = PathBuf::from(r"D:\shoots\finals");
        assert_eq!(
            relative_to_root(&root, Path::new(r"D:\shoots\finals\day1\IMG_0001.jpg")).as_deref(),
            Some("day1/IMG_0001.jpg")
        );
        assert_eq!(
            relative_to_root(&root, Path::new(r"D:\shoots\finals\IMG_0001.jpg")).as_deref(),
            Some("IMG_0001.jpg")
        );
    }

    #[test]
    fn a_file_outside_the_root_is_not_portable() {
        let root = PathBuf::from(r"D:\shoots\finals");
        assert_eq!(relative_to_root(&root, Path::new(r"E:\elsewhere\IMG.jpg")), None);
        assert_eq!(relative_to_root(&root, &root), None, "the root itself is not a file");
    }

    #[test]
    fn share_roots_keep_their_own_separator() {
        assert_eq!(
            join_share(r"\\STUDIO-PC\shoots\finals\", "day1/IMG_0001.jpg"),
            r"\\STUDIO-PC\shoots\finals\day1\IMG_0001.jpg"
        );
        assert_eq!(
            join_share("/Volumes/shoots/finals", "day1/IMG_0001.jpg"),
            "/Volumes/shoots/finals/day1/IMG_0001.jpg"
        );
    }

    #[test]
    fn client_path_needs_both_halves() {
        assert_eq!(
            client_path(&shoot(Some(r"\\STUDIO-PC\shoots\finals")), &media(Some("day1/IMG_0001.jpg"))).as_deref(),
            Some(r"\\STUDIO-PC\shoots\finals\day1\IMG_0001.jpg")
        );
        assert_eq!(client_path(&shoot(None), &media(Some("day1/IMG_0001.jpg"))), None);
        assert_eq!(client_path(&shoot(Some(r"\\STUDIO-PC\shoots")), &media(None)), None);
        assert_eq!(client_path(&shoot(Some("   ")), &media(Some("a.jpg"))), None);
    }
}
