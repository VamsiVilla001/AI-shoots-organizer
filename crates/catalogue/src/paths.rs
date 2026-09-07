use std::path::{Component, Path, PathBuf};

use crate::{CatalogueError, Result};

pub fn normalize_relative_path(path: &Path) -> Result<String> {
    if path.is_absolute() {
        return Err(CatalogueError::Invalid("absolute media reference rejected".into()));
    }
    let mut safe = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(value) => {
                let text = value
                    .to_str()
                    .ok_or_else(|| CatalogueError::Invalid("path is not valid UTF-8".into()))?;
                if text.contains(':') || text.contains('\0') {
                    return Err(CatalogueError::Invalid("device path rejected".into()));
                }
                safe.push(text);
            }
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(CatalogueError::Invalid("path traversal rejected".into()));
            }
        }
    }
    if safe.is_empty() {
        return Err(CatalogueError::Invalid("empty media reference".into()));
    }
    Ok(safe.join("/"))
}

pub fn resolve_beneath_root(root: &Path, relative: &str) -> Result<PathBuf> {
    if relative.contains("//") || relative.contains("\\") || relative.contains("://") {
        return Err(CatalogueError::Invalid("unsafe media reference".into()));
    }
    let normalized = normalize_relative_path(Path::new(relative))?;
    Ok(root.join(normalized.replace('/', std::path::MAIN_SEPARATOR_STR)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalises_portable_references() {
        assert_eq!(
            normalize_relative_path(Path::new("day1/clip.mov")).unwrap(),
            "day1/clip.mov"
        );
    }

    #[test]
    fn rejects_absolute_and_traversal_paths() {
        assert!(normalize_relative_path(Path::new("../secret.mov")).is_err());
        assert!(resolve_beneath_root(Path::new("D:/nas"), "C:/outside.mov").is_err());
        assert!(resolve_beneath_root(Path::new("D:/nas"), "https://bad.test/x").is_err());
    }
}
