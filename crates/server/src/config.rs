//! Where the server's configuration comes from, in order: command line, then
//! environment, then a config file.
//!
//! The file matters because a Windows service is started by the Service
//! Control Manager with a bare environment block — nothing an operator set in
//! a shell reaches it. `skwad-server install` writes the file from the flags
//! it was given, and the service reads it back. Environment wins per value so
//! a developer can override one setting against an installed service without
//! editing the file; the command line wins over both.
//!
//! The file is `KEY=VALUE` lines, the same shape `services/backend` uses.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

/// Overrides where the config file is read from.
pub const CONFIG_PATH_VAR: &str = "SKWAD_SERVER_CONFIG";

/// The API contract version this build speaks. A client must send it as
/// `X-Skwad-Api`; anything else is `426 Upgrade Required`.
pub const API_VERSION: u32 = skwad_app_core::work_api::API_VERSION;

#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub bind: SocketAddr,
    /// The library folder: models, auth, caches, `database.json`.
    pub library_root: PathBuf,
    /// Rebuildable caches (thumbnails, proxies, frames) may live elsewhere.
    pub cache_root: Option<PathBuf>,
    /// `postgres://…`. `None` resolves `database.json` in the library root
    /// the way the desktop does.
    pub database_url: Option<String>,
    /// Directories a shoot source may live under. The filesystem browser is
    /// jailed to these; a scan outside them is refused.
    pub media_roots: Vec<PathBuf>,
    /// PEM certificate and key. Both or neither.
    pub tls_cert: Option<PathBuf>,
    pub tls_key: Option<PathBuf>,
    /// Origins allowed to call the API from a page this server did not serve
    /// (a desktop client's webview, the Vite dev server).
    pub allowed_origins: Vec<String>,
    /// A built React bundle to serve at `/`, for browser clients.
    pub web_dir: Option<PathBuf>,
    /// Where the machine half of the settings lives on this box.
    pub machine_settings_file: Option<PathBuf>,
    /// How long a signed-in session lasts without use.
    pub session_ttl_hours: u64,
    /// How many AI workers this box runs itself.
    pub ai_workers: Option<usize>,
    /// Whether this box analyses files at all. Off for a server without a
    /// GPU: it still scans, indexes and runs the finishing stages, and every
    /// analysis job waits for an enrolled worker machine.
    pub local_analysis: bool,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind: "127.0.0.1:8420".parse().expect("static address"),
            library_root: default_library_root(),
            cache_root: None,
            database_url: None,
            media_roots: Vec::new(),
            tls_cert: None,
            tls_key: None,
            allowed_origins: Vec::new(),
            web_dir: None,
            machine_settings_file: None,
            session_ttl_hours: 24 * 14,
            ai_workers: None,
            local_analysis: true,
        }
    }
}

/// Where the library lives when nobody said: beside the service's other state.
pub fn default_library_root() -> PathBuf {
    program_data().join("SKWAD").join("library")
}

pub fn default_config_path() -> PathBuf {
    if let Some(explicit) = std::env::var_os(CONFIG_PATH_VAR) {
        return PathBuf::from(explicit);
    }
    program_data().join("SKWAD").join("server.env")
}

pub fn default_log_dir() -> PathBuf {
    program_data().join("SKWAD").join("logs")
}

fn program_data() -> PathBuf {
    #[cfg(windows)]
    {
        PathBuf::from(std::env::var_os("PROGRAMDATA").unwrap_or_else(|| r"C:\ProgramData".into()))
    }
    #[cfg(not(windows))]
    {
        PathBuf::from("/var/lib")
    }
}

/// Parses `KEY=VALUE` lines, ignoring blanks and `#` comments. Values run to
/// the end of the line, so a Windows path with spaces needs no quoting.
pub fn parse_env_file(contents: &str) -> HashMap<String, String> {
    contents
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(|line| line.split_once('='))
        .map(|(key, value)| (key.trim().to_string(), value.trim().to_string()))
        .collect()
}

/// Splits a path list on `;` (Windows convention) and `,`, but not on the
/// colon inside `C:\`.
pub fn parse_path_list(raw: &str) -> Vec<PathBuf> {
    raw.split([';', ','])
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(PathBuf::from)
        .collect()
}

/// The keys the config file and environment understand.
pub const KEYS: &[&str] = &[
    "SKWAD_SERVER_BIND",
    "SKWAD_SERVER_LIBRARY",
    "SKWAD_SERVER_CACHE",
    "SKWAD_DATABASE_URL",
    "SKWAD_SERVER_MEDIA_ROOTS",
    "SKWAD_SERVER_TLS_CERT",
    "SKWAD_SERVER_TLS_KEY",
    "SKWAD_SERVER_ALLOWED_ORIGINS",
    "SKWAD_SERVER_WEB_DIR",
    "SKWAD_SERVER_MACHINE_SETTINGS",
    "SKWAD_SERVER_SESSION_TTL_HOURS",
    "SKWAD_SERVER_AI_WORKERS",
    "SKWAD_SERVER_LOCAL_ANALYSIS",
];

/// Layered lookup: command line, environment, file.
pub struct Sources {
    cli: HashMap<String, String>,
    file: HashMap<String, String>,
    pub file_path: PathBuf,
    pub file_was_read: bool,
}

impl Sources {
    pub fn discover(cli: HashMap<String, String>) -> Self {
        let file_path = default_config_path();
        let (file, file_was_read) = match std::fs::read_to_string(&file_path) {
            Ok(contents) => (parse_env_file(&contents), true),
            Err(_) => (HashMap::new(), false),
        };
        Self {
            cli,
            file,
            file_path,
            file_was_read,
        }
    }

    pub fn get(&self, key: &str) -> Option<String> {
        self.cli
            .get(key)
            .cloned()
            .or_else(|| std::env::var(key).ok())
            .or_else(|| self.file.get(key).cloned())
            .filter(|value| !value.trim().is_empty())
    }
}

impl ServerConfig {
    /// Resolves the configuration from every source. `cli` holds the same
    /// keys as the file, already parsed from flags by `main`.
    pub fn resolve(cli: HashMap<String, String>) -> anyhow::Result<Self> {
        let sources = Sources::discover(cli);
        let mut config = Self::default();

        if let Some(bind) = sources.get("SKWAD_SERVER_BIND") {
            config.bind = bind
                .parse()
                .map_err(|e| anyhow::anyhow!("SKWAD_SERVER_BIND `{bind}` is not host:port: {e}"))?;
        }
        if let Some(root) = sources.get("SKWAD_SERVER_LIBRARY") {
            config.library_root = PathBuf::from(root);
        }
        config.cache_root = sources.get("SKWAD_SERVER_CACHE").map(PathBuf::from);
        config.database_url = sources.get("SKWAD_DATABASE_URL");
        config.media_roots = sources
            .get("SKWAD_SERVER_MEDIA_ROOTS")
            .map(|raw| parse_path_list(&raw))
            .unwrap_or_default();
        config.tls_cert = sources.get("SKWAD_SERVER_TLS_CERT").map(PathBuf::from);
        config.tls_key = sources.get("SKWAD_SERVER_TLS_KEY").map(PathBuf::from);
        if config.tls_cert.is_some() != config.tls_key.is_some() {
            anyhow::bail!("SKWAD_SERVER_TLS_CERT and SKWAD_SERVER_TLS_KEY must be set together");
        }
        config.allowed_origins = sources
            .get("SKWAD_SERVER_ALLOWED_ORIGINS")
            .map(|raw| {
                raw.split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(String::from)
                    .collect()
            })
            .unwrap_or_default();
        config.web_dir = sources.get("SKWAD_SERVER_WEB_DIR").map(PathBuf::from);
        config.machine_settings_file = sources.get("SKWAD_SERVER_MACHINE_SETTINGS").map(PathBuf::from);
        if let Some(hours) = sources.get("SKWAD_SERVER_SESSION_TTL_HOURS") {
            config.session_ttl_hours = hours
                .parse()
                .map_err(|e| anyhow::anyhow!("SKWAD_SERVER_SESSION_TTL_HOURS `{hours}`: {e}"))?;
        }
        if let Some(workers) = sources.get("SKWAD_SERVER_AI_WORKERS") {
            config.ai_workers = Some(
                workers
                    .parse()
                    .map_err(|e| anyhow::anyhow!("SKWAD_SERVER_AI_WORKERS `{workers}`: {e}"))?,
            );
        }
        if let Some(flag) = sources.get("SKWAD_SERVER_LOCAL_ANALYSIS") {
            config.local_analysis = match flag.trim().to_ascii_lowercase().as_str() {
                "1" | "true" | "yes" | "on" => true,
                "0" | "false" | "no" | "off" => false,
                other => anyhow::bail!("SKWAD_SERVER_LOCAL_ANALYSIS `{other}`: expected true or false"),
            };
        }

        if !sources.file_was_read {
            tracing::debug!(path = %sources.file_path.display(), "no config file; using environment and defaults");
        }
        Ok(config)
    }

    pub fn tls_enabled(&self) -> bool {
        self.tls_cert.is_some()
    }

    /// Renders the values back into file form, for `install`.
    pub fn render_env_file(&self) -> String {
        let mut out = String::from(
            "# SKWAD server configuration. Written by `skwad-server install`.\n\
             # A matching environment variable overrides any line here; a command-line\n\
             # flag overrides both.\n\n",
        );
        let mut line = |key: &str, value: Option<String>| {
            if let Some(value) = value {
                out.push_str(key);
                out.push('=');
                out.push_str(&value);
                out.push('\n');
            }
        };
        line("SKWAD_SERVER_BIND", Some(self.bind.to_string()));
        line("SKWAD_SERVER_LIBRARY", Some(self.library_root.display().to_string()));
        line("SKWAD_SERVER_CACHE", self.cache_root.as_ref().map(|p| p.display().to_string()));
        line("SKWAD_DATABASE_URL", self.database_url.clone());
        line(
            "SKWAD_SERVER_MEDIA_ROOTS",
            (!self.media_roots.is_empty()).then(|| {
                self.media_roots
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join(";")
            }),
        );
        line("SKWAD_SERVER_TLS_CERT", self.tls_cert.as_ref().map(|p| p.display().to_string()));
        line("SKWAD_SERVER_TLS_KEY", self.tls_key.as_ref().map(|p| p.display().to_string()));
        line(
            "SKWAD_SERVER_ALLOWED_ORIGINS",
            (!self.allowed_origins.is_empty()).then(|| self.allowed_origins.join(",")),
        );
        line("SKWAD_SERVER_WEB_DIR", self.web_dir.as_ref().map(|p| p.display().to_string()));
        line(
            "SKWAD_SERVER_MACHINE_SETTINGS",
            self.machine_settings_file.as_ref().map(|p| p.display().to_string()),
        );
        line("SKWAD_SERVER_SESSION_TTL_HOURS", Some(self.session_ttl_hours.to_string()));
        line("SKWAD_SERVER_AI_WORKERS", self.ai_workers.map(|n| n.to_string()));
        line("SKWAD_SERVER_LOCAL_ANALYSIS", Some(self.local_analysis.to_string()));
        out
    }
}

/// Why a path was refused by the jail.
#[derive(Debug)]
pub enum JailError {
    NoRoots,
    Unreadable(String),
    Outside(String),
}

impl std::fmt::Display for JailError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            JailError::NoRoots => write!(f, "no media roots are configured; set SKWAD_SERVER_MEDIA_ROOTS"),
            JailError::Unreadable(p) => write!(f, "{p} cannot be read"),
            JailError::Outside(p) => write!(f, "{p} is outside the folders this server may browse"),
        }
    }
}

impl std::error::Error for JailError {}

/// Canonicalises `requested` and confirms it sits inside one of `roots`.
/// `..`, symlinks and absolute paths elsewhere all fail closed.
pub fn resolve_within_roots(requested: &Path, roots: &[PathBuf]) -> Result<PathBuf, JailError> {
    if roots.is_empty() {
        return Err(JailError::NoRoots);
    }
    let canonical = std::fs::canonicalize(requested)
        .map_err(|_| JailError::Unreadable(requested.display().to_string()))?;
    for root in roots {
        let Ok(root) = std::fs::canonicalize(root) else { continue };
        if canonical.starts_with(&root) {
            return Ok(without_verbatim_prefix(canonical));
        }
    }
    Err(JailError::Outside(requested.display().to_string()))
}

/// `canonicalize` on Windows answers with an extended-length path
/// (`\\?\C:\…`, `\\?\UNC\server\share\…`). That form is what the OS wants,
/// not what a person or a share mapping does: it goes into shoot rows and
/// onto screens, so it is turned back into the ordinary spelling.
pub fn without_verbatim_prefix(path: PathBuf) -> PathBuf {
    let text = path.to_string_lossy();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{rest}"));
    }
    if let Some(rest) = text.strip_prefix(r"\\?\") {
        return PathBuf::from(rest);
    }
    path
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_files_parse_like_the_backend_ones() {
        let parsed = parse_env_file("# note\nSKWAD_SERVER_BIND=0.0.0.0:8420\nSKWAD_SERVER_MEDIA_ROOTS = D:\\shoots;E:\\more \nnope\n");
        assert_eq!(parsed["SKWAD_SERVER_BIND"], "0.0.0.0:8420");
        assert_eq!(parsed["SKWAD_SERVER_MEDIA_ROOTS"], "D:\\shoots;E:\\more");
        assert_eq!(parsed.len(), 2);
    }

    #[test]
    fn verbatim_prefixes_are_removed_from_resolved_paths() {
        assert_eq!(
            without_verbatim_prefix(PathBuf::from(r"\\?\C:\shoots\day1")),
            PathBuf::from(r"C:\shoots\day1")
        );
        assert_eq!(
            without_verbatim_prefix(PathBuf::from(r"\\?\UNC\nas\share\day1")),
            PathBuf::from(r"\\nas\share\day1")
        );
        assert_eq!(without_verbatim_prefix(PathBuf::from(r"D:\plain")), PathBuf::from(r"D:\plain"));
    }

    #[test]
    fn path_lists_keep_drive_letters() {
        assert_eq!(
            parse_path_list("C:\\a;D:\\b, E:\\c"),
            vec![PathBuf::from("C:\\a"), PathBuf::from("D:\\b"), PathBuf::from("E:\\c")]
        );
    }

    #[test]
    fn the_jail_refuses_outside_and_missing_paths() {
        let inside = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let roots = vec![inside.path().to_path_buf()];
        assert!(resolve_within_roots(inside.path(), &roots).is_ok());
        assert!(matches!(
            resolve_within_roots(outside.path(), &roots),
            Err(JailError::Outside(_))
        ));
        assert!(matches!(
            resolve_within_roots(&inside.path().join("missing"), &roots),
            Err(JailError::Unreadable(_))
        ));
        assert!(matches!(resolve_within_roots(inside.path(), &[]), Err(JailError::NoRoots)));
    }

    #[test]
    fn a_rendered_file_reads_back() {
        let config = ServerConfig {
            media_roots: vec![PathBuf::from("D:\\shoots")],
            tls_cert: Some(PathBuf::from("C:\\certs\\s.pem")),
            tls_key: Some(PathBuf::from("C:\\certs\\k.pem")),
            ..Default::default()
        };
        let parsed = parse_env_file(&config.render_env_file());
        assert_eq!(parsed["SKWAD_SERVER_BIND"], "127.0.0.1:8420");
        assert_eq!(parsed["SKWAD_SERVER_MEDIA_ROOTS"], "D:\\shoots");
        assert_eq!(parsed["SKWAD_SERVER_TLS_CERT"], "C:\\certs\\s.pem");
        assert!(!parsed.contains_key("SKWAD_SERVER_WEB_DIR"));
    }
}
