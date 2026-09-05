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
        self.logs.join("teo.log")
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
        let temp = std::env::temp_dir().join(format!("teo-paths-test-{}", std::process::id()));
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
}
