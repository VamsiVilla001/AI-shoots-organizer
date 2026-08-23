//! Export (§11).
//!
//! The end of the workflow: take the albums the application produced and lay
//! the *original* files out in folders an editor can open. The one hard rule
//! is that source media is never modified, moved or renamed in place — every
//! operation here writes to the destination folder and only reads from source.

pub mod naming;

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub use naming::{deduplicate, sanitise_component};

#[derive(Debug, thiserror::Error)]
pub enum ExportError {
    #[error("destination is not writable: {0}")]
    Destination(String),
    #[error("refusing to export into the shoot's own source folder")]
    DestinationInsideSource,
    #[error("io error on {path}: {message}")]
    Io { path: String, message: String },
    #[error("export was cancelled")]
    Cancelled,
}

pub type Result<T> = std::result::Result<T, ExportError>;

/// What to do when the destination already holds a file of that name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ExistingFilePolicy {
    /// Leave it alone and count it as done — makes re-running an export cheap.
    Skip,
    /// Write alongside it as `name (2).jpg`.
    Rename,
    Overwrite,
}

/// Where the output folders come from.
///
/// `Groups` is the normal path: the editor named the folders in the app and the
/// export writes exactly those. `AiAlbums` exports the recognition results
/// directly, for the case where nothing needs correcting by hand.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ExportMode {
    Groups,
    AiAlbums,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportOptions {
    /// Whether the folders come from the editor's groups or the AI albums.
    pub mode: ExportMode,
    /// Restrict a `Groups` export to these groups. `None` exports all of them.
    pub group_ids: Option<Vec<i64>>,
    /// Split each folder into `Photos/` and `Videos/` (§11).
    pub split_photos_videos: bool,
    /// `AiAlbums` only: include the unidentified album as its own folder.
    pub include_unidentified: bool,
    /// `AiAlbums` only: restrict the export to these players. `None` exports
    /// everyone.
    pub person_ids: Option<Vec<i64>>,
    /// Copy access and modification times onto the exported file.
    pub preserve_metadata: bool,
    pub existing: ExistingFilePolicy,
    /// `AiAlbums` only: also write multi-player albums as their own folders.
    pub include_multi_player: bool,
    /// `AiAlbums` only: also write the group-size albums ("Single", "Two
    /// persons", …) as folders. Off by default: every file is in both a player
    /// album and a group-size album, so enabling this writes the shoot twice.
    pub include_group_size: bool,
    /// Write a `_sorting-report.txt` beside the folders listing what went
    /// where — an editor handing the NAS folder to someone else needs a
    /// manifest, and it is the only file the export creates itself.
    pub write_manifest: bool,
}

impl Default for ExportOptions {
    fn default() -> Self {
        Self {
            mode: ExportMode::Groups,
            group_ids: None,
            split_photos_videos: true,
            include_unidentified: true,
            person_ids: None,
            preserve_metadata: true,
            existing: ExistingFilePolicy::Skip,
            include_multi_player: false,
            include_group_size: false,
            write_manifest: true,
        }
    }
}

/// One file to be exported, and where it lands.
#[derive(Debug, Clone, PartialEq)]
pub struct ExportItem {
    pub source: PathBuf,
    /// Path relative to the destination root.
    pub relative: PathBuf,
    pub size: u64,
}

#[derive(Debug, Clone, Default)]
pub struct ExportPlan {
    pub items: Vec<ExportItem>,
    /// Folders that will be created, in the order they appear.
    pub folders: Vec<String>,
}

impl ExportPlan {
    pub fn total_bytes(&self) -> u64 {
        self.items.iter().map(|i| i.size).sum()
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

/// A source file destined for one album folder.
#[derive(Debug, Clone)]
pub struct SourceFile {
    pub path: PathBuf,
    pub filename: String,
    pub is_video: bool,
    pub size: u64,
}

/// One output folder — a player, a pairing, or "Unidentified".
#[derive(Debug, Clone)]
pub struct ExportGroup {
    pub name: String,
    pub files: Vec<SourceFile>,
}

/// Works out the full set of writes before touching the disk, so the UI can
/// show a file count and byte total up front and the run is all-or-nothing in
/// terms of surprises.
pub fn plan(groups: &[ExportGroup], options: &ExportOptions) -> ExportPlan {
    let mut plan = ExportPlan::default();

    for group in groups {
        if group.files.is_empty() {
            continue;
        }
        let folder = sanitise_component(&group.name);

        // Names are deduplicated per destination folder, not globally: two
        // players may each have their own IMG_0231.JPG and both should keep it.
        let mut taken_photos: HashSet<String> = HashSet::new();
        let mut taken_videos: HashSet<String> = HashSet::new();

        for file in &group.files {
            let subfolder = if options.split_photos_videos {
                Some(if file.is_video { "Videos" } else { "Photos" })
            } else {
                None
            };

            let taken = if options.split_photos_videos && file.is_video {
                &mut taken_videos
            } else {
                &mut taken_photos
            };
            let filename = deduplicate(&file.filename, taken);

            let relative = match subfolder {
                Some(sub) => PathBuf::from(&folder).join(sub).join(&filename),
                None => PathBuf::from(&folder).join(&filename),
            };

            let folder_key = relative
                .parent()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default();
            if !plan.folders.contains(&folder_key) {
                plan.folders.push(folder_key);
            }

            plan.items.push(ExportItem {
                source: file.path.clone(),
                relative,
                size: file.size,
            });
        }
    }

    plan
}

/// Rejects destinations that would write into the shoot's own folder, which
/// would both pollute the source and make a re-scan pick up the copies.
///
/// Two checks, because a path comparison is not enough on a server:
///
/// 1. **By path**, after canonicalising, so `..` and symlinks cannot dress up
///    a destination as being somewhere else.
/// 2. **By file identity**, because a container commonly mounts the same share
///    twice — `/media` read-only and `/output` writable — and those are
///    different paths that canonicalise differently while being the same
///    directory. Comparing the device and inode catches that; it is the only
///    way to see through a bind mount.
///
/// The identity check is Unix-only: `std::os::windows` exposes a file index
/// only behind an unstable feature. On Windows the path check stands alone,
/// which is what the desktop app has always relied on.
pub fn validate_destination(destination: &Path, source_roots: &[PathBuf]) -> Result<()> {
    let canonical_destination = normalise(destination);

    for root in source_roots {
        let canonical_root = normalise(root);
        if canonical_destination == canonical_root || canonical_destination.starts_with(&canonical_root) {
            return Err(ExportError::DestinationInsideSource);
        }

        // The destination itself, or any ancestor of it, being the same
        // directory as the source means the copies would land in the source
        // through another mount point.
        let mut candidate = Some(canonical_destination.as_path());
        while let Some(path) = candidate {
            if is_same_directory(path, &canonical_root) {
                return Err(ExportError::DestinationInsideSource);
            }
            candidate = path.parent();
        }
    }

    Ok(())
}

/// Whether two paths name the same directory on disk, regardless of how they
/// were reached.
#[cfg(unix)]
fn is_same_directory(a: &Path, b: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;

    let (Ok(a), Ok(b)) = (std::fs::metadata(a), std::fs::metadata(b)) else {
        return false;
    };
    a.is_dir() && b.is_dir() && a.dev() == b.dev() && a.ino() == b.ino()
}

#[cfg(not(unix))]
fn is_same_directory(a: &Path, b: &Path) -> bool {
    // No stable file identity here, so this collapses to the path check the
    // caller has already made.
    a == b
}

fn normalise(path: &Path) -> PathBuf {
    // canonicalize only works on paths that exist; fall back to the raw path so
    // validation still runs for a folder the user is about to create.
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ExportProgress {
    pub files_done: usize,
    pub files_skipped: usize,
    pub bytes_done: u64,
}

/// Executes a plan, copying originals into place.
///
/// `should_continue` is polled between files so the UI can cancel a long
/// export; `on_progress` reports after each one.
pub fn execute(
    plan: &ExportPlan,
    destination: &Path,
    options: &ExportOptions,
    mut should_continue: impl FnMut() -> bool,
    mut on_progress: impl FnMut(ExportProgress),
) -> Result<ExportProgress> {
    std::fs::create_dir_all(destination)
        .map_err(|e| ExportError::Destination(format!("{}: {e}", destination.display())))?;

    let mut progress = ExportProgress::default();

    for item in &plan.items {
        if !should_continue() {
            return Err(ExportError::Cancelled);
        }

        let target = destination.join(&item.relative);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|e| ExportError::Io {
                path: parent.display().to_string(),
                message: e.to_string(),
            })?;
        }

        let final_target = match (target.exists(), options.existing) {
            (true, ExistingFilePolicy::Skip) => {
                progress.files_skipped += 1;
                on_progress(progress);
                continue;
            }
            (true, ExistingFilePolicy::Rename) => next_free_name(&target),
            _ => target,
        };

        std::fs::copy(&item.source, &final_target).map_err(|e| ExportError::Io {
            path: item.source.display().to_string(),
            message: e.to_string(),
        })?;

        if options.preserve_metadata {
            // Best effort: a destination that cannot hold timestamps (some
            // network shares) should not fail the export.
            if let Ok(meta) = std::fs::metadata(&item.source) {
                if let Ok(modified) = meta.modified() {
                    let time = filetime::FileTime::from_system_time(modified);
                    if let Err(e) = filetime::set_file_mtime(&final_target, time) {
                        tracing::debug!(path = %final_target.display(), error = %e, "could not preserve mtime");
                    }
                }
            }
        }

        progress.files_done += 1;
        progress.bytes_done += item.size;
        on_progress(progress);
    }

    if options.write_manifest {
        // Best effort: a manifest that could not be written is worth a log
        // line, not a failed export of files that already copied fine.
        if let Err(e) = write_manifest(plan, destination) {
            tracing::warn!(error = %e, "could not write the sorting report");
        }
    }

    Ok(progress)
}

/// The name of the one file an export creates that is not a copy of a source
/// file. Prefixed with `_` so it sorts above the group folders.
pub const MANIFEST_FILENAME: &str = "_sorting-report.txt";

/// Writes a plain-text list of every folder and the files in it, with the
/// source path each one came from. This is what makes an exported NAS folder
/// self-describing once it leaves the app.
pub fn write_manifest(plan: &ExportPlan, destination: &Path) -> std::io::Result<()> {
    use std::fmt::Write as _;

    let mut report = String::new();
    let _ = writeln!(report, "Esports AI Media Organiser — sorting report");
    let _ = writeln!(report, "Destination : {}", destination.display());
    let _ = writeln!(report, "Folders     : {}", plan.folders.len());
    let _ = writeln!(report, "Files       : {}", plan.items.len());
    let _ = writeln!(report, "\nOriginals were copied. The source folder was not modified.\n");

    let mut current = String::new();
    for item in &plan.items {
        let folder = item
            .relative
            .parent()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();
        if folder != current {
            let _ = writeln!(report, "\n[{folder}]");
            current = folder;
        }
        let name = item.relative.file_name().unwrap_or_default().to_string_lossy();
        let _ = writeln!(report, "  {name}\t<- {}", item.source.display());
    }

    std::fs::write(destination.join(MANIFEST_FILENAME), report)
}

fn next_free_name(target: &Path) -> PathBuf {
    let parent = target.parent().unwrap_or(Path::new("."));
    let stem = target.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let extension = target.extension().map(|e| e.to_string_lossy().to_string());

    for n in 2..10_000 {
        let candidate = match &extension {
            Some(ext) => parent.join(format!("{stem} ({n}).{ext}")),
            None => parent.join(format!("{stem} ({n})")),
        };
        if !candidate.exists() {
            return candidate;
        }
    }
    target.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(name: &str, is_video: bool) -> SourceFile {
        SourceFile {
            path: PathBuf::from(format!("C:\\shoot\\{name}")),
            filename: name.to_string(),
            is_video,
            size: 1024,
        }
    }

    fn groups() -> Vec<ExportGroup> {
        vec![
            ExportGroup {
                name: "Jonathan".into(),
                files: vec![file("IMG_0231.JPG", false), file("Final.mp4", true)],
            },
            ExportGroup {
                name: "Mavi".into(),
                files: vec![file("IMG_0231.JPG", false)],
            },
        ]
    }

    #[test]
    fn plans_the_folder_structure_from_the_spec() {
        let plan = plan(&groups(), &ExportOptions::default());
        let relatives: Vec<String> = plan
            .items
            .iter()
            .map(|i| i.relative.to_string_lossy().replace('\\', "/"))
            .collect();

        assert!(relatives.contains(&"Jonathan/Photos/IMG_0231.JPG".to_string()));
        assert!(relatives.contains(&"Jonathan/Videos/Final.mp4".to_string()));
        assert!(relatives.contains(&"Mavi/Photos/IMG_0231.JPG".to_string()));
        assert_eq!(plan.total_bytes(), 3 * 1024);
    }

    #[test]
    fn photos_and_videos_can_share_one_folder() {
        let options = ExportOptions { split_photos_videos: false, ..Default::default() };
        let plan = plan(&groups(), &options);
        let relatives: Vec<String> = plan
            .items
            .iter()
            .map(|i| i.relative.to_string_lossy().replace('\\', "/"))
            .collect();
        assert!(relatives.contains(&"Jonathan/IMG_0231.JPG".to_string()));
        assert!(relatives.contains(&"Jonathan/Final.mp4".to_string()));
    }

    #[test]
    fn the_same_filename_in_two_players_folders_is_not_renamed() {
        // Deduplication is per folder, so both players keep the original name.
        let plan = plan(&groups(), &ExportOptions::default());
        let count = plan
            .items
            .iter()
            .filter(|i| i.relative.file_name().unwrap() == "IMG_0231.JPG")
            .count();
        assert_eq!(count, 2);
    }

    #[test]
    fn duplicates_within_one_folder_are_renamed() {
        let groups = vec![ExportGroup {
            name: "Jonathan".into(),
            files: vec![file("IMG_0231.JPG", false), file("IMG_0231.JPG", false)],
        }];
        let plan = plan(&groups, &ExportOptions::default());
        let names: Vec<String> = plan
            .items
            .iter()
            .map(|i| i.relative.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert_eq!(names, vec!["IMG_0231.JPG", "IMG_0231 (2).JPG"]);
    }

    #[test]
    fn unsafe_player_names_become_safe_folders() {
        let groups = vec![ExportGroup {
            name: "Team/Player: A?".into(),
            files: vec![file("a.jpg", false)],
        }];
        let plan = plan(&groups, &ExportOptions::default());
        let path = plan.items[0].relative.to_string_lossy().replace('\\', "/");
        assert!(path.starts_with("Team_Player_ A_/"), "got {path}");
    }

    #[test]
    fn empty_groups_are_dropped() {
        let groups = vec![ExportGroup { name: "Nobody".into(), files: vec![] }];
        assert!(plan(&groups, &ExportOptions::default()).is_empty());
    }

    #[test]
    fn the_default_export_is_driven_by_the_editors_groups() {
        // The product's normal path: whatever the editor named in the app is
        // what lands on disk. Nothing about `plan` is album-specific.
        assert_eq!(ExportOptions::default().mode, ExportMode::Groups);
        let plan = plan(
            &[ExportGroup {
                name: "Day 2 — Jonathan".into(),
                files: vec![file("IMG_0231.JPG", false)],
            }],
            &ExportOptions::default(),
        );
        let path = plan.items[0].relative.to_string_lossy().replace('\\', "/");
        assert_eq!(path, "Day 2 — Jonathan/Photos/IMG_0231.JPG");
    }

    #[test]
    fn the_manifest_lists_every_folder_and_its_source_paths() {
        let source_dir = tempfile::tempdir().unwrap();
        let dest_dir = tempfile::tempdir().unwrap();
        let source_file = source_dir.path().join("IMG_0231.JPG");
        std::fs::write(&source_file, b"bytes").unwrap();

        let plan = ExportPlan {
            items: vec![ExportItem {
                source: source_file,
                relative: PathBuf::from("Jonathan").join("Photos").join("IMG_0231.JPG"),
                size: 5,
            }],
            folders: vec!["Jonathan/Photos".into()],
        };

        execute(&plan, dest_dir.path(), &ExportOptions::default(), || true, |_| {}).unwrap();

        let report = std::fs::read_to_string(dest_dir.path().join(MANIFEST_FILENAME)).unwrap();
        assert!(report.contains("Jonathan"));
        assert!(report.contains("IMG_0231.JPG"));
        assert!(report.contains("was not modified"));
    }

    #[test]
    fn the_manifest_can_be_turned_off() {
        let source_dir = tempfile::tempdir().unwrap();
        let dest_dir = tempfile::tempdir().unwrap();
        let source_file = source_dir.path().join("a.jpg");
        std::fs::write(&source_file, b"x").unwrap();

        let plan = ExportPlan {
            items: vec![ExportItem {
                source: source_file,
                relative: PathBuf::from("Jonathan").join("a.jpg"),
                size: 1,
            }],
            folders: vec!["Jonathan".into()],
        };
        let options = ExportOptions {
            write_manifest: false,
            ..Default::default()
        };
        execute(&plan, dest_dir.path(), &options, || true, |_| {}).unwrap();
        assert!(!dest_dir.path().join(MANIFEST_FILENAME).exists());
    }

    #[test]
    fn a_second_mount_of_the_source_is_still_the_source() {
        // What a NAS container does: the same directory presented twice, once
        // as the read-only source and once as the writable destination. The two
        // paths differ and canonicalise differently, so only file identity can
        // tell they are one directory.
        let share = tempfile::tempdir().unwrap();
        let source = share.path().join("shoot");
        std::fs::create_dir_all(&source).unwrap();

        // A symlink is the portable stand-in for a bind mount here: a real bind
        // mount needs root, and both produce a second path to one directory.
        let second_view = share.path().join("output");
        #[cfg(unix)]
        let linked = std::os::unix::fs::symlink(&source, &second_view).is_ok();
        #[cfg(windows)]
        let linked = std::os::windows::fs::symlink_dir(&source, &second_view).is_ok();

        if !linked {
            return; // Windows without the privilege; the path check still applies.
        }

        assert!(
            validate_destination(&second_view, std::slice::from_ref(&source)).is_err(),
            "a second path to the source folder must be refused"
        );

        // And a genuinely separate folder on the same share is still fine.
        let elsewhere = share.path().join("sorted");
        std::fs::create_dir_all(&elsewhere).unwrap();
        assert!(validate_destination(&elsewhere, std::slice::from_ref(&source)).is_ok());
    }

    #[test]
    fn exporting_into_the_source_folder_is_refused() {
        let source = PathBuf::from("C:\\BGMS_Final_Shoot");
        assert!(validate_destination(Path::new("C:\\BGMS_Final_Shoot"), std::slice::from_ref(&source)).is_err());
        assert!(validate_destination(Path::new("C:\\BGMS_Final_Shoot\\Export"), std::slice::from_ref(&source)).is_err());
        assert!(validate_destination(Path::new("D:\\Export"), &[source]).is_ok());
    }

    #[test]
    fn execute_copies_files_and_leaves_the_source_alone() {
        let source_dir = tempfile::tempdir().unwrap();
        let dest_dir = tempfile::tempdir().unwrap();

        let source_file = source_dir.path().join("IMG_0231.JPG");
        std::fs::write(&source_file, b"original bytes").unwrap();

        let plan = ExportPlan {
            items: vec![ExportItem {
                source: source_file.clone(),
                relative: PathBuf::from("Jonathan").join("Photos").join("IMG_0231.JPG"),
                size: 14,
            }],
            folders: vec!["Jonathan/Photos".into()],
        };

        let progress = execute(&plan, dest_dir.path(), &ExportOptions::default(), || true, |_| {}).unwrap();
        assert_eq!(progress.files_done, 1);

        let exported = dest_dir.path().join("Jonathan").join("Photos").join("IMG_0231.JPG");
        assert_eq!(std::fs::read(&exported).unwrap(), b"original bytes");
        // The source must be untouched and still present.
        assert_eq!(std::fs::read(&source_file).unwrap(), b"original bytes");
    }

    #[test]
    fn rerunning_an_export_skips_what_is_already_there() {
        let source_dir = tempfile::tempdir().unwrap();
        let dest_dir = tempfile::tempdir().unwrap();
        let source_file = source_dir.path().join("a.jpg");
        std::fs::write(&source_file, b"x").unwrap();

        let plan = ExportPlan {
            items: vec![ExportItem {
                source: source_file,
                relative: PathBuf::from("Jonathan").join("a.jpg"),
                size: 1,
            }],
            folders: vec!["Jonathan".into()],
        };
        let options = ExportOptions { existing: ExistingFilePolicy::Skip, ..Default::default() };

        assert_eq!(execute(&plan, dest_dir.path(), &options, || true, |_| {}).unwrap().files_done, 1);
        let second = execute(&plan, dest_dir.path(), &options, || true, |_| {}).unwrap();
        assert_eq!(second.files_done, 0);
        assert_eq!(second.files_skipped, 1);
    }

    #[test]
    fn cancellation_stops_the_run() {
        let source_dir = tempfile::tempdir().unwrap();
        let dest_dir = tempfile::tempdir().unwrap();
        let source_file = source_dir.path().join("a.jpg");
        std::fs::write(&source_file, b"x").unwrap();

        let plan = ExportPlan {
            items: vec![ExportItem {
                source: source_file,
                relative: PathBuf::from("Jonathan").join("a.jpg"),
                size: 1,
            }],
            folders: vec![],
        };

        let result = execute(&plan, dest_dir.path(), &ExportOptions::default(), || false, |_| {});
        assert!(matches!(result, Err(ExportError::Cancelled)));
    }
}
