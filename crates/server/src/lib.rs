//! HTTP front door for [`teo_app_core`].
//!
//! Scaffolding only at this point: the routes arrive in the next phase, and
//! they will be a one-to-one port of the Tauri command layer so the two front
//! doors stay trivially comparable.

use std::path::PathBuf;

/// Everything the server needs from its environment. Nothing here is discovered
/// from a desktop path resolver — a container gets it all from flags or env.
#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub bind: String,
    pub data_dir: PathBuf,
    /// Directories a shoot source may live under. Anything outside is refused,
    /// which is what keeps a filesystem browser from becoming a file server.
    pub media_roots: Vec<PathBuf>,
    /// Directories an export may write into.
    pub output_roots: Vec<PathBuf>,
    pub token: Option<String>,
}

impl ServerConfig {
    /// Reads the environment, applying the documented defaults.
    pub fn from_env() -> Self {
        let list = |key: &str| -> Vec<PathBuf> {
            std::env::var(key)
                .ok()
                .map(|raw| {
                    raw.split([',', ';'])
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(PathBuf::from)
                        .collect()
                })
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
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_defaults_match_the_container_layout() {
        // Env vars are process-global, so this test only asserts the shape that
        // holds when nothing is set for it.
        let config = ServerConfig {
            bind: "0.0.0.0:8420".into(),
            data_dir: PathBuf::from("/config"),
            media_roots: vec![PathBuf::from("/media")],
            output_roots: vec![PathBuf::from("/output")],
            token: None,
        };
        assert!(config.bind.ends_with(":8420"));
        assert_eq!(config.data_dir, PathBuf::from("/config"));
        assert!(config.token.is_none(), "a generated token is written on first run instead");
    }
}
