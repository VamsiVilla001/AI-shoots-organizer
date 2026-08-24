//! Server configuration, and the path jail every filesystem route goes through.

use std::path::{Path, PathBuf};

/// Everything the server needs from its environment. Nothing here is discovered
/// from a desktop path resolver — a container gets it all from flags or env.
#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub bind: String,
    pub data_dir: PathBuf,
    /// Directories a shoot source may live under. Anything outside is refused,
    /// which is what keeps a filesystem browser from becoming a file server.
    pub media_roots: Vec<PathBuf>,
    /// Directories an export may write into. Empty means "the media roots",
    /// which is only sensible for a local run.
    pub output_roots: Vec<PathBuf>,
    pub token: Option<String>,
    /// The built React bundle. Missing is not fatal — the API still serves.
    pub web_dir: Option<PathBuf>,
    /// Where to write the address actually bound, for a parent process that
    /// asked for port 0 and needs to know what it got.
    pub port_file: Option<PathBuf>,
    /// A directory of ONNX models to install into the data directory on first
    /// run. The desktop shell points this at its bundle resources so a packaged
    /// app recognises faces without anyone fetching anything.
    pub seed_models_from: Option<PathBuf>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind: "0.0.0.0:8420".into(),
            data_dir: PathBuf::from("/config"),
            media_roots: Vec::new(),
            output_roots: Vec::new(),
            token: None,
            web_dir: None,
            port_file: None,
            seed_models_from: None,
        }
    }
}

impl ServerConfig {
    /// Reads the environment, applying the documented defaults.
    pub fn from_env() -> Self {
        let list = |key: &str| -> Vec<PathBuf> {
            std::env::var(key)
                .ok()
                .map(|raw| parse_path_list(&raw))
                .unwrap_or_default()
        };

        Self {
            bind: std::env::var("TEO_BIND").unwrap_or_else(|_| "0.0.0.0:8420".into()),
            data_dir: std::env::var("TEO_DATA_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("/config")),
            media_roots: list("TEO_MEDIA_ROOTS"),
            output_roots: list("TEO_OUTPUT_ROOTS"),
            token: std::env::var("TEO_TOKEN").ok().filter(|t| !t.trim().is_empty()),
            web_dir: std::env::var("TEO_WEB_DIR").ok().map(PathBuf::from),
            port_file: std::env::var("TEO_PORT_FILE").ok().map(PathBuf::from),
            seed_models_from: std::env::var("TEO_SEED_MODELS_FROM").ok().map(PathBuf::from),
        }
    }

    /// Roots an export may write into. Falling back to the media roots keeps a
    /// single-folder local run usable without configuring two variables.
    pub fn writable_roots(&self) -> &[PathBuf] {
        if self.output_roots.is_empty() {
            &self.media_roots
        } else {
            &self.output_roots
        }
    }
}

/// Splits a `TEO_*_ROOTS` value. Semicolons as well as commas, because a
/// Windows path list is conventionally semicolon-separated — and a bare `C:\x`
/// would otherwise be unsplittable.
pub fn parse_path_list(raw: &str) -> Vec<PathBuf> {
    let separator = if raw.contains(';') { ';' } else { ',' };
    raw.split(separator)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .collect()
}

/// Strips Windows' extended-length prefix from a canonicalised path.
///
/// `Path::canonicalize` hands back `\\?\C:\…` on Windows. Every API call
/// accepts it, but it leaks into everything a person reads — an export report,
/// an error message, a folder picker — and some external tools reject it
/// outright, so it comes off before a path leaves this module.
pub fn tidy(path: PathBuf) -> PathBuf {
    if !cfg!(windows) {
        return path;
    }

    let text = path.to_string_lossy().to_string();
    // A UNC share comes back as `\\?\UNC\server\share`, which has to become
    // `\\server\share` rather than losing a leading slash.
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{rest}"));
    }
    match text.strip_prefix(r"\\?\") {
        Some(rest) => PathBuf::from(rest),
        None => path,
    }
}

#[derive(Debug, thiserror::Error)]
pub enum JailError {
    #[error("no media roots are configured, so no folder can be browsed")]
    NoRoots,
    #[error("{0} does not exist or cannot be read")]
    Unreadable(String),
    #[error("{0} is outside the folders this server is allowed to touch")]
    Outside(String),
}

/// Canonicalises `requested` and confirms it sits inside one of `roots`.
///
/// Canonicalising first is the whole point: it resolves `..` and symlinks, so a
/// path that *looks* contained cannot escape. A path that does not exist is
/// refused rather than guessed at — for a browser that is correct, and for an
/// export destination the caller creates the folder first.
pub fn resolve_within_roots(requested: &Path, roots: &[PathBuf]) -> Result<PathBuf, JailError> {
    if roots.is_empty() {
        return Err(JailError::NoRoots);
    }

    let canonical = requested
        .canonicalize()
        .map_err(|_| JailError::Unreadable(requested.display().to_string()))?;

    for root in roots {
        let Ok(root) = root.canonicalize() else { continue };
        if canonical == root || canonical.starts_with(&root) {
            return Ok(tidy(canonical));
        }
    }

    Err(JailError::Outside(requested.display().to_string()))
}

/// Like [`resolve_within_roots`], but for a destination that may not exist yet:
/// the deepest existing ancestor has to be inside a root, and the rest is
/// created under it.
pub fn resolve_new_within_roots(requested: &Path, roots: &[PathBuf]) -> Result<PathBuf, JailError> {
    if roots.is_empty() {
        return Err(JailError::NoRoots);
    }
    if requested.exists() {
        return resolve_within_roots(requested, roots);
    }

    let mut existing = requested.parent();
    while let Some(ancestor) = existing {
        if ancestor.exists() {
            // The ancestor is real, so check *it*, then re-attach the tail.
            let anchor = resolve_within_roots(ancestor, roots)?;
            let tail = requested
                .strip_prefix(ancestor)
                .map_err(|_| JailError::Outside(requested.display().to_string()))?;
            return Ok(tidy(anchor.join(tail)));
        }
        existing = ancestor.parent();
    }

    Err(JailError::Unreadable(requested.display().to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_semicolon_list_survives_windows_drive_letters() {
        let roots = parse_path_list(r"C:\media;D:\archive");
        assert_eq!(roots.len(), 2);
        assert_eq!(roots[1], PathBuf::from(r"D:\archive"));

        let commas = parse_path_list("/media, /output");
        assert_eq!(commas, vec![PathBuf::from("/media"), PathBuf::from("/output")]);
    }

    #[test]
    fn a_path_inside_a_root_resolves() {
        let root = tempfile::tempdir().unwrap();
        let inner = root.path().join("shoot/day 2");
        std::fs::create_dir_all(&inner).unwrap();

        let roots = vec![root.path().to_path_buf()];
        let resolved = resolve_within_roots(&inner, &roots).unwrap();
        assert!(resolved.ends_with("day 2"));
    }

    #[test]
    fn traversal_out_of_a_root_is_refused() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("shoot")).unwrap();

        let roots = vec![root.path().join("shoot")];

        // `..` is resolved before the check, so this lands outside the root.
        let escape = root.path().join("shoot").join("..");
        assert!(matches!(
            resolve_within_roots(&escape, &roots),
            Err(JailError::Outside(_))
        ));

        assert!(matches!(
            resolve_within_roots(outside.path(), &roots),
            Err(JailError::Outside(_))
        ));
    }

    #[test]
    fn a_symlink_pointing_out_of_a_root_is_refused() {
        let root = tempfile::tempdir().unwrap();
        let secret = tempfile::tempdir().unwrap();
        let link = root.path().join("escape");

        // Symlink creation needs privileges on Windows; skip rather than fail.
        #[cfg(unix)]
        let made = std::os::unix::fs::symlink(secret.path(), &link).is_ok();
        #[cfg(windows)]
        let made = std::os::windows::fs::symlink_dir(secret.path(), &link).is_ok();

        if !made {
            return;
        }

        let roots = vec![root.path().to_path_buf()];
        assert!(
            matches!(resolve_within_roots(&link, &roots), Err(JailError::Outside(_))),
            "canonicalisation must resolve the link before the containment check"
        );
    }

    #[test]
    fn the_windows_extended_length_prefix_is_stripped() {
        if !cfg!(windows) {
            return;
        }
        assert_eq!(tidy(PathBuf::from(r"\\?\C:\media\shoot")), PathBuf::from(r"C:\media\shoot"));
        // A UNC share keeps both leading slashes.
        assert_eq!(
            tidy(PathBuf::from(r"\\?\UNC\NAS\Editors\Day 2")),
            PathBuf::from(r"\\NAS\Editors\Day 2")
        );
        // An ordinary path is untouched.
        assert_eq!(tidy(PathBuf::from(r"D:\shoots")), PathBuf::from(r"D:\shoots"));
    }

    #[test]
    fn a_resolved_path_is_free_of_the_extended_length_prefix() {
        let root = tempfile::tempdir().unwrap();
        let inner = root.path().join("shoot");
        std::fs::create_dir_all(&inner).unwrap();

        let resolved = resolve_within_roots(&inner, &[root.path().to_path_buf()]).unwrap();
        assert!(
            !resolved.to_string_lossy().starts_with(r"\\?\"),
            "callers see a plain path: {}",
            resolved.display()
        );
    }

    #[test]
    fn no_roots_means_nothing_is_browsable() {
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(resolve_within_roots(dir.path(), &[]), Err(JailError::NoRoots)));
    }

    #[test]
    fn a_destination_that_does_not_exist_yet_is_checked_by_its_parent() {
        let root = tempfile::tempdir().unwrap();
        let roots = vec![root.path().to_path_buf()];

        let target = root.path().join("exports/BGMS/Sorted");
        let resolved = resolve_new_within_roots(&target, &roots).unwrap();
        assert!(resolved.ends_with("Sorted"));

        let outside = tempfile::tempdir().unwrap().path().join("nested/deeper");
        assert!(resolve_new_within_roots(&outside, &roots).is_err());
    }
}
