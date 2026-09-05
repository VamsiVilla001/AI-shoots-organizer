//! Content-addressed, application-managed video proxy cache.
//!
//! Proxies are complete, low-resolution viewing copies. They never replace or
//! modify the source video and can always be regenerated.

use std::path::{Path, PathBuf};

use crate::{MediaError, Result};

pub const VIDEO_PROXY_WIDTH: u32 = 512;

#[derive(Debug, Clone)]
pub struct VideoProxyCache {
    root: PathBuf,
}

impl VideoProxyCache {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn path_for(&self, content_key: &str) -> PathBuf {
        let safe_key: String = content_key
            .chars()
            .filter(|character| character.is_ascii_alphanumeric())
            .take(80)
            .collect();
        let key = if safe_key.is_empty() { "unknown" } else { &safe_key };
        let shard = key.get(..2).unwrap_or("00");
        self.root.join(shard).join(format!("{key}-proxy-v1.mp4"))
    }

    pub fn clear(&self) -> Result<u64> {
        if !self.root.exists() {
            return Ok(0);
        }
        let mut removed = 0;
        for entry in walkdir::WalkDir::new(&self.root).into_iter().flatten() {
            if entry.file_type().is_file() && std::fs::remove_file(entry.path()).is_ok() {
                removed += 1;
            }
        }
        Ok(removed)
    }

    pub fn remove(&self, content_key: &str) -> Result<bool> {
        let path = self.path_for(content_key);
        if !path.is_file() {
            return Ok(false);
        }
        std::fs::remove_file(&path).map_err(|error| MediaError::Io(format!("remove {}: {error}", path.display())))?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proxies_are_sharded_inside_their_own_root() {
        let cache = VideoProxyCache::new("C:\\data\\proxies");
        let path = cache.path_for("ab12-cd34");
        assert!(path.starts_with(cache.root()));
        assert!(path.ends_with(Path::new("ab").join("ab12cd34-proxy-v1.mp4")));
    }

    #[test]
    fn clear_removes_proxy_files() {
        let directory = tempfile::tempdir().unwrap();
        let cache = VideoProxyCache::new(directory.path());
        let path = cache.path_for("aa01");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"proxy").unwrap();
        assert_eq!(cache.clear().unwrap(), 1);
        assert!(!path.exists());
    }
}
