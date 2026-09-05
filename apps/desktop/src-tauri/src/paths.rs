//! Application-managed storage layout (§17).
//!
//! ```text
//! AppData/
//! ├── database/media.db
//! ├── thumbnails/
//! ├── proxies/
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
    pub proxies: PathBuf,
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
            proxies: root.join("proxies"),
            face_cache: root.join("face_cache"),
            models: root.join("models"),
            logs: root.join("logs"),
            root,
        };

        for dir in [
            &paths.database,
            &paths.thumbnails,
            &paths.proxies,
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
        [&self.thumbnails, &self.proxies, &self.face_cache]
            .iter()
            .map(|dir| directory_size(dir))
            .sum()
    }
}

/// Bundle identifier used by releases before the SKWAD V2 rebrand.
const LEGACY_IDENTIFIER: &str = "com.teorganiser.desktop";
const LEGACY_BACKUP: &str = "media.db.pre-skwad-v2.bak";
const MOVED_NOTE: &str = "moved-to-com.skwad.mediaorganiser.txt";

#[derive(Debug, PartialEq, Eq)]
pub enum LegacyMigration {
    NotNeeded,
    Moved(PathBuf),
    Copied(PathBuf),
}

/// Move an established pre-SKWAD application library to the new bundle data
/// directory. Only app-managed indexes and caches are touched; shoot/NAS media
/// paths are never modified. A database backup is made before any move, and a
/// failed verification is rolled back to the original directory.
pub fn migrate_legacy_data_dir(new_root: &Path) -> std::io::Result<LegacyMigration> {
    if new_root.join("database").join("media.db").is_file() {
        return Ok(LegacyMigration::NotNeeded);
    }
    let Some(parent) = new_root.parent() else {
        return Ok(LegacyMigration::NotNeeded);
    };
    let legacy = parent.join(LEGACY_IDENTIFIER);
    let legacy_db = legacy.join("database").join("media.db");
    if !legacy_db.is_file() {
        return Ok(LegacyMigration::NotNeeded);
    }

    let backup = legacy.join("database").join(LEGACY_BACKUP);
    if !backup.exists() {
        std::fs::copy(&legacy_db, &backup)?;
        verify_copy(&legacy_db, &backup)?;
    }

    if !new_root.exists() {
        std::fs::rename(&legacy, new_root)?;
        let migrated_db = new_root.join("database").join("media.db");
        if let Err(error) = verify_copy(&new_root.join("database").join(LEGACY_BACKUP), &migrated_db) {
            let _ = std::fs::rename(new_root, &legacy);
            return Err(error);
        }
        leave_moved_note(&legacy, new_root);
        return Ok(LegacyMigration::Moved(legacy));
    }

    copy_tree(&legacy, new_root)?;
    verify_copy(&legacy_db, &new_root.join("database").join("media.db"))?;
    Ok(LegacyMigration::Copied(legacy))
}

fn verify_copy(source: &Path, destination: &Path) -> std::io::Result<()> {
    let source_len = std::fs::metadata(source)?.len();
    let destination_len = std::fs::metadata(destination)?.len();
    if source_len == 0 || source_len != destination_len {
        return Err(std::io::Error::other("the migrated database did not verify"));
    }
    Ok(())
}

fn copy_tree(source: &Path, destination: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(destination)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let target = destination.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else if !target.exists() {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

fn leave_moved_note(legacy: &Path, new_root: &Path) {
    if std::fs::create_dir_all(legacy).is_ok() {
        let _ = std::fs::write(
            legacy.join(MOVED_NOTE),
            format!(
                "SKWAD Media Organiser moved its app-managed library to:\n{}\n\nThe original database was backed up before migration. NAS media was not changed.\n",
                new_root.display()
            ),
        );
    }
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
        assert!(paths.proxies.is_dir());
        assert!(paths.face_cache.is_dir());
        assert!(paths.models.is_dir());
        assert!(paths.logs.is_dir());
        assert!(paths.database_file().ends_with("media.db"));

        // Creating twice must not fail.
        AppPaths::create(&temp).unwrap();

        std::fs::remove_dir_all(&temp).ok();
    }

    #[test]
    fn face_crops_are_sharded() {
        let paths = AppPaths {
            root: PathBuf::from("/data"),
            database: PathBuf::from("/data/database"),
            thumbnails: PathBuf::from("/data/thumbnails"),
            proxies: PathBuf::from("/data/proxies"),
            face_cache: PathBuf::from("/data/face_cache"),
            models: PathBuf::from("/data/models"),
            logs: PathBuf::from("/data/logs"),
        };
        assert!(paths.face_crop(1234).ends_with(Path::new("34").join("1234.jpg")));
        assert!(paths.face_crop(7).ends_with(Path::new("07").join("7.jpg")));
    }

    fn seed_legacy(parent: &Path) -> PathBuf {
        let legacy = parent.join(LEGACY_IDENTIFIER);
        std::fs::create_dir_all(legacy.join("database")).unwrap();
        std::fs::create_dir_all(legacy.join("proxies")).unwrap();
        std::fs::write(legacy.join("database").join("media.db"), b"sqlite-library").unwrap();
        std::fs::write(legacy.join("proxies").join("preview.mp4"), b"proxy").unwrap();
        legacy
    }

    #[test]
    fn legacy_library_is_backed_up_and_moved() {
        let parent = tempfile::tempdir().unwrap();
        let legacy = seed_legacy(parent.path());
        let new_root = parent.path().join("com.skwad.mediaorganiser");

        assert_eq!(
            migrate_legacy_data_dir(&new_root).unwrap(),
            LegacyMigration::Moved(legacy.clone())
        );
        assert!(new_root.join("database").join("media.db").is_file());
        assert!(new_root.join("database").join(LEGACY_BACKUP).is_file());
        assert!(new_root.join("proxies").join("preview.mp4").is_file());
        assert!(legacy.join(MOVED_NOTE).is_file());
    }

    #[test]
    fn an_existing_skwad_library_is_never_overwritten() {
        let parent = tempfile::tempdir().unwrap();
        seed_legacy(parent.path());
        let new_root = parent.path().join("com.skwad.mediaorganiser");
        std::fs::create_dir_all(new_root.join("database")).unwrap();
        std::fs::write(new_root.join("database").join("media.db"), b"current").unwrap();

        assert_eq!(migrate_legacy_data_dir(&new_root).unwrap(), LegacyMigration::NotNeeded);
        assert_eq!(
            std::fs::read(new_root.join("database").join("media.db")).unwrap(),
            b"current"
        );
    }
}
