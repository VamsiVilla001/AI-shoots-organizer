//! Application-managed storage layout (§17).
//!
//! ```text
//! AppData/
//! ├── database/media.db
//! ├── thumbnails/
//! ├── face_cache/
//! ├── models/
//! └── logs/
//! ```
//!
//! The user's media stays where it is; nothing in this tree is ever written
//! back to a shoot folder.

use std::path::{Path, PathBuf};

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppPaths {
    pub root: PathBuf,
    pub database: PathBuf,
    pub thumbnails: PathBuf,
    pub face_cache: PathBuf,
    pub models: PathBuf,
    pub logs: PathBuf,
}

impl AppPaths {
    /// Builds the layout under `root` and creates every directory.
    pub fn create(root: impl AsRef<Path>) -> std::io::Result<Self> {
        let root = root.as_ref().to_path_buf();
        let paths = Self {
            database: root.join("database"),
            thumbnails: root.join("thumbnails"),
            face_cache: root.join("face_cache"),
            models: root.join("models"),
            logs: root.join("logs"),
            root,
        };

        for dir in [
            &paths.database,
            &paths.thumbnails,
            &paths.face_cache,
            &paths.models,
            &paths.logs,
        ] {
            std::fs::create_dir_all(dir)?;
        }

        Ok(paths)
    }

    pub fn database_file(&self) -> PathBuf {
        self.database.join("media.db")
    }

    pub fn log_file(&self) -> PathBuf {
        self.logs.join("skwad.log")
    }

    /// Where a face crop is cached, sharded the same way thumbnails are.
    pub fn face_crop(&self, face_id: i64) -> PathBuf {
        let shard = format!("{:02}", face_id.unsigned_abs() % 100);
        self.face_cache.join(shard).join(format!("{face_id}.jpg"))
    }

    /// Total bytes used by the caches, for the Settings screen.
    pub fn cache_size(&self) -> u64 {
        [&self.thumbnails, &self.face_cache]
            .iter()
            .map(|dir| directory_size(dir))
            .sum()
    }
}

/// The identifier this application shipped under before it was renamed to SKWAD
/// Media Organiser.
///
/// Tauri derives the data directory from the bundle identifier, so renaming the
/// application moved the entire library: `media.db`, the thumbnail cache, the
/// face cache, the downloaded models and the logs. Without this the renamed
/// build would open on an empty database and every shoot, person and confirmed
/// face would look lost — while sitting untouched in a folder next door.
const LEGACY_IDENTIFIER: &str = "com.teorganiser.desktop";

/// Note left behind at the old location, so anyone who goes looking for the
/// library finds out where it went rather than concluding it was deleted.
const MOVED_NOTE: &str = "moved-to-com.skwad.mediaorganiser.txt";

#[derive(Debug, PartialEq, Eq)]
pub enum Migration {
    /// Either there is already a library here, or there was never an old one.
    NotNeeded,
    /// The old folder was renamed onto the new path.
    Moved(PathBuf),
    /// The new folder already existed, so the contents were copied and the old
    /// folder was left in place.
    Copied(PathBuf),
}

/// Brings a pre-rename library across to the new data directory.
///
/// Both identifiers resolve to siblings — `%APPDATA%\Roaming\<id>` on Windows,
/// `~/Library/Application Support/<id>` on macOS — so the old folder is found
/// relative to the new one rather than by rebuilding a platform path.
///
/// A rename is preferred over a copy: the library routinely runs to gigabytes
/// of thumbnails, and stalling the first launch to duplicate all of it would be
/// worse than the move. Nothing is ever deleted; if the rename cannot be done
/// the contents are copied and the original is left exactly as it was.
pub fn migrate_legacy_data_dir(new_root: &Path) -> std::io::Result<Migration> {
    // An existing database here means this install is already established, and
    // anything found next door is older history. Never overwrite it.
    if new_root.join("database").join("media.db").is_file() {
        return Ok(Migration::NotNeeded);
    }

    let Some(legacy) = new_root.parent().map(|parent| parent.join(LEGACY_IDENTIFIER)) else {
        return Ok(Migration::NotNeeded);
    };
    // The database is the test of a real library. An empty folder left by an
    // uninstall is not worth migrating.
    if !legacy.join("database").join("media.db").is_file() {
        return Ok(Migration::NotNeeded);
    }

    if !new_root.exists() {
        if let Some(parent) = new_root.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if std::fs::rename(&legacy, new_root).is_ok() {
            leave_moved_note(&legacy, new_root);
            return Ok(Migration::Moved(legacy));
        }
        // Fall through: a rename can fail across volumes or while a file is
        // held open, and a copy still gets the library across.
    }

    copy_tree(&legacy, new_root)?;
    Ok(Migration::Copied(legacy))
}

/// Best effort, and deliberately ignored on failure: the migration has already
/// succeeded by this point and must not be reported as failed over a note.
fn leave_moved_note(legacy: &Path, new_root: &Path) {
    if std::fs::create_dir_all(legacy).is_ok() {
        let _ = std::fs::write(
            legacy.join(MOVED_NOTE),
            format!(
                "SKWAD Media Organiser was previously called TE Organiser, and its data\n\
                 directory moved with the rename.\n\n\
                 This library is now at:\n  {}\n\n\
                 Nothing was deleted; the folder was moved. This empty folder can be\n\
                 removed.\n",
                new_root.display()
            ),
        );
    }
}

fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else if !target.exists() {
            // Never clobber a file already at the destination.
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

fn directory_size(dir: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| match entry.file_type() {
            Ok(t) if t.is_dir() => directory_size(&entry.path()),
            Ok(_) => entry.metadata().map(|m| m.len()).unwrap_or(0),
            Err(_) => 0,
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_the_full_layout() {
        let temp = std::env::temp_dir().join(format!("skwad-paths-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&temp);

        let paths = AppPaths::create(&temp).unwrap();
        assert!(paths.database.is_dir());
        assert!(paths.thumbnails.is_dir());
        assert!(paths.face_cache.is_dir());
        assert!(paths.models.is_dir());
        assert!(paths.logs.is_dir());
        assert!(paths.database_file().ends_with("media.db"));

        // Creating twice must not fail.
        AppPaths::create(&temp).unwrap();

        std::fs::remove_dir_all(&temp).ok();
    }

    /// A pre-rename library: the database is what makes it real, and the model
    /// files are what make migrating it worth 280 MB of not re-downloading.
    fn seed_legacy(parent: &Path) -> PathBuf {
        let legacy = parent.join(LEGACY_IDENTIFIER);
        std::fs::create_dir_all(legacy.join("database")).unwrap();
        std::fs::create_dir_all(legacy.join("thumbnails").join("ab")).unwrap();
        std::fs::create_dir_all(legacy.join("models")).unwrap();
        std::fs::write(legacy.join("database").join("media.db"), b"sqlite").unwrap();
        std::fs::write(legacy.join("thumbnails").join("ab").join("abc.jpg"), b"jpeg").unwrap();
        std::fs::write(legacy.join("models").join("det_10g.onnx"), b"onnx").unwrap();
        legacy
    }

    #[test]
    fn an_old_library_is_moved_onto_the_new_identifier() {
        let temp = tempfile::tempdir().unwrap();
        let legacy = seed_legacy(temp.path());
        let new_root = temp.path().join("com.skwad.mediaorganiser");

        let outcome = migrate_legacy_data_dir(&new_root).unwrap();
        assert_eq!(outcome, Migration::Moved(legacy.clone()));

        // Everything arrived, nested files included.
        assert!(new_root.join("database").join("media.db").is_file());
        assert!(new_root.join("thumbnails").join("ab").join("abc.jpg").is_file());
        assert!(new_root.join("models").join("det_10g.onnx").is_file());
        // And the old location explains itself rather than looking deleted.
        assert!(legacy.join(MOVED_NOTE).is_file());
        assert!(!legacy.join("database").join("media.db").exists());
    }

    #[test]
    fn an_existing_library_is_never_overwritten() {
        // The dangerous case: someone has been using the renamed build for a
        // week when an older folder is still lying next to it.
        let temp = tempfile::tempdir().unwrap();
        seed_legacy(temp.path());
        let new_root = temp.path().join("com.skwad.mediaorganiser");
        std::fs::create_dir_all(new_root.join("database")).unwrap();
        std::fs::write(new_root.join("database").join("media.db"), b"current").unwrap();

        assert_eq!(migrate_legacy_data_dir(&new_root).unwrap(), Migration::NotNeeded);
        assert_eq!(
            std::fs::read(new_root.join("database").join("media.db")).unwrap(),
            b"current"
        );
    }

    #[test]
    fn a_new_folder_that_already_exists_is_filled_by_copying() {
        // Tauri or an earlier launch may have created the directory before the
        // migration runs, which rules out a rename.
        let temp = tempfile::tempdir().unwrap();
        let legacy = seed_legacy(temp.path());
        let new_root = temp.path().join("com.skwad.mediaorganiser");
        std::fs::create_dir_all(new_root.join("logs")).unwrap();

        assert_eq!(
            migrate_legacy_data_dir(&new_root).unwrap(),
            Migration::Copied(legacy.clone())
        );
        assert!(new_root.join("database").join("media.db").is_file());
        assert!(new_root.join("thumbnails").join("ab").join("abc.jpg").is_file());
        // A copy leaves the original in place.
        assert!(legacy.join("database").join("media.db").is_file());
    }

    #[test]
    fn a_fresh_install_with_no_history_migrates_nothing() {
        let temp = tempfile::tempdir().unwrap();
        let new_root = temp.path().join("com.skwad.mediaorganiser");
        assert_eq!(migrate_legacy_data_dir(&new_root).unwrap(), Migration::NotNeeded);
        assert!(!new_root.exists());
    }

    #[test]
    fn an_empty_old_folder_is_not_worth_migrating() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(temp.path().join(LEGACY_IDENTIFIER).join("logs")).unwrap();
        let new_root = temp.path().join("com.skwad.mediaorganiser");

        assert_eq!(migrate_legacy_data_dir(&new_root).unwrap(), Migration::NotNeeded);
    }

    #[test]
    fn face_crops_are_sharded() {
        let paths = AppPaths {
            root: PathBuf::from("/data"),
            database: PathBuf::from("/data/database"),
            thumbnails: PathBuf::from("/data/thumbnails"),
            face_cache: PathBuf::from("/data/face_cache"),
            models: PathBuf::from("/data/models"),
            logs: PathBuf::from("/data/logs"),
        };
        assert!(paths.face_crop(1234).ends_with(Path::new("34").join("1234.jpg")));
        assert!(paths.face_crop(7).ends_with(Path::new("07").join("7.jpg")));
    }
}
