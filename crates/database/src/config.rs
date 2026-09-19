//! Where the database lives, and how the app is told.
//!
//! Under SQLite this was a `PathBuf` — the library folder held `media.db` and
//! that was the whole of it. A server needs host, port, database, credentials
//! and a TLS decision, and those come from three places in a fixed order:
//!
//!   1. `SKWAD_DATABASE_URL`, which wins outright. This is what CI, the
//!      migration tool and a developer pointing at a scratch database use.
//!   2. `database.json` in the library folder. Written by Settings, so an
//!      operator pointing an edit bay at the studio server does it once in the
//!      UI and every launch afterwards finds it.
//!   3. The built-in default: the `skwad` database on `localhost:5432`, which
//!      is what the bundled local server provisions.
//!
//! The password is deliberately *not* written to `database.json`: it is read
//! from `SKWAD_DATABASE_PASSWORD` or, failing that, from the standard
//! `~/.pgpass`, so a shared library folder never carries a credential.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::{DbError, Result};

/// The file Settings writes into the library folder.
const CONFIG_FILE: &str = "database.json";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PgConfig {
    pub host: String,
    pub port: u16,
    pub database: String,
    pub user: String,
    /// Never serialised — see the module note.
    #[serde(skip)]
    pub password: Option<String>,
    /// `max_size` of the r2d2 pool.
    #[serde(default = "default_max_connections")]
    pub max_connections: u32,
    #[serde(default = "default_connect_timeout_secs")]
    pub connect_timeout_secs: u64,
    /// A schema to keep this library in, instead of `public`. Lets one
    /// server hold several libraries (a studio's and a test one) under one
    /// database; `?schema=name` on the URL sets it.
    #[serde(default)]
    pub schema: Option<String>,
}

fn default_max_connections() -> u32 {
    8
}

fn default_connect_timeout_secs() -> u64 {
    10
}

/// The TCP host of a parsed connection string. A Unix-socket host only
/// exists on Unix builds, and the app always connects over TCP, so it is
/// passed over; on Windows the match has one arm, which is why this is a
/// function rather than a closure clippy would want simplified.
fn tcp_host(host: &postgres::config::Host) -> Option<String> {
    match host {
        postgres::config::Host::Tcp(host) => Some(host.clone()),
        #[cfg(unix)]
        postgres::config::Host::Unix(_) => None,
    }
}

impl Default for PgConfig {
    fn default() -> Self {
        Self {
            host: "localhost".to_string(),
            port: 5432,
            database: "skwad".to_string(),
            user: "skwad".to_string(),
            password: None,
            max_connections: default_max_connections(),
            connect_timeout_secs: default_connect_timeout_secs(),
            schema: None,
        }
    }
}

impl PgConfig {
    /// Resolves the configuration for the library rooted at `library_root`,
    /// applying the precedence described on this module.
    pub fn resolve(library_root: &Path) -> Self {
        if let Ok(url) = std::env::var("SKWAD_DATABASE_URL") {
            match Self::from_url(&url) {
                Ok(config) => return config.with_env_password(),
                // A malformed override is worth saying out loud rather than
                // silently falling through to a different server.
                Err(error) => tracing::warn!(%error, "ignoring an invalid SKWAD_DATABASE_URL"),
            }
        }

        let path = library_root.join(CONFIG_FILE);
        if let Ok(raw) = std::fs::read_to_string(&path) {
            match serde_json::from_str::<Self>(&raw) {
                Ok(config) => return config.with_env_password(),
                Err(error) => tracing::warn!(%error, path = %path.display(), "ignoring an unreadable database.json"),
            }
        }

        Self::default().with_env_password()
    }

    /// Persists everything but the password into the library folder.
    pub fn save(&self, library_root: &Path) -> Result<()> {
        let path = library_root.join(CONFIG_FILE);
        let raw = serde_json::to_string_pretty(self)?;
        std::fs::write(&path, raw).map_err(|e| DbError::other(format!("write {}: {e}", path.display())))
    }

    fn with_env_password(mut self) -> Self {
        if self.password.is_none() {
            self.password = std::env::var("SKWAD_DATABASE_PASSWORD")
                .ok()
                .or_else(|| self.password_from_pgpass());
        }
        self
    }

    /// Reads the password from the standard PostgreSQL password file.
    ///
    /// rust-postgres, unlike libpq, does not consult this itself, so it is done
    /// here. Keeping the mechanism standard means `psql` and the migration tool
    /// pick up the same credential the app does, and the password stays out of
    /// the library folder where a shared drive would expose it.
    ///
    /// Format is libpq's: `host:port:database:user:password`, one per line,
    /// `*` matching anything, `\` escaping a literal `:` or `\`.
    fn password_from_pgpass(&self) -> Option<String> {
        let path = match std::env::var_os("PGPASSFILE") {
            Some(path) => PathBuf::from(path),
            // libpq uses %APPDATA%\postgresql\pgpass.conf on Windows and
            // ~/.pgpass elsewhere.
            None if cfg!(windows) => PathBuf::from(std::env::var_os("APPDATA")?)
                .join("postgresql")
                .join("pgpass.conf"),
            None => PathBuf::from(std::env::var_os("HOME")?).join(".pgpass"),
        };

        let contents = std::fs::read_to_string(path).ok()?;
        for line in contents.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let fields = split_pgpass_line(line);
            let [host, port, database, user, password] = fields.as_slice() else {
                continue;
            };
            let matches = |field: &str, value: &str| field == "*" || field == value;
            if matches(host, &self.host)
                && matches(port, &self.port.to_string())
                && matches(database, &self.database)
                && matches(user, &self.user)
            {
                return Some(password.clone());
            }
        }
        None
    }

    /// Parses a `postgres://user:password@host:port/database` URL.
    pub fn from_url(url: &str) -> Result<Self> {
        // `schema` is ours, not libpq's: take it off before the parser sees it.
        let (url, schema) = split_schema(url)?;
        let parsed: postgres::Config = url
            .parse()
            .map_err(|e| DbError::other(format!("not a PostgreSQL connection string: {e}")))?;

        let defaults = Self::default();
        Ok(Self {
            host: parsed
                .get_hosts()
                .iter()
                .find_map(tcp_host)
                .unwrap_or(defaults.host),
            port: parsed.get_ports().first().copied().unwrap_or(defaults.port),
            database: parsed.get_dbname().unwrap_or(&defaults.database).to_string(),
            user: parsed.get_user().unwrap_or(&defaults.user).to_string(),
            password: parsed
                .get_password()
                .map(|bytes| String::from_utf8_lossy(bytes).into_owned()),
            max_connections: defaults.max_connections,
            connect_timeout_secs: defaults.connect_timeout_secs,
            schema,
        })
    }

    pub fn connect_timeout(&self) -> Duration {
        Duration::from_secs(self.connect_timeout_secs)
    }

    pub fn to_postgres_config(&self) -> postgres::Config {
        let mut config = postgres::Config::new();
        config
            .host(&self.host)
            .port(self.port)
            .dbname(&self.database)
            .user(&self.user)
            .connect_timeout(self.connect_timeout())
            // The worker pool holds connections for the length of a job; without
            // this a dropped network library wedges a worker until the OS
            // notices, which on Windows is minutes.
            .keepalives(true);
        if let Some(password) = &self.password {
            config.password(password);
        }
        if let Some(schema) = &self.schema {
            config.options(&format!("-c search_path={schema}"));
        }
        config
    }

    /// A human-readable identifier for the Settings screen and logs. Contains
    /// no credential, so it is safe to log.
    pub fn describe(&self) -> String {
        match &self.schema {
            Some(schema) => format!("{}@{}:{}/{}/{schema}", self.user, self.host, self.port, self.database),
            None => format!("{}@{}:{}/{}", self.user, self.host, self.port, self.database),
        }
    }

    /// True when this points at the machine the app is running on, which is
    /// what decides whether the app should offer to start the bundled server.
    pub fn is_local(&self) -> bool {
        matches!(self.host.as_str(), "localhost" | "127.0.0.1" | "::1")
    }
}

/// Splits one `pgpass` line on unescaped colons, honouring `\:` and `\\`.
///
/// Passwords containing a colon are exactly why this is not `line.split(':')`.
fn split_pgpass_line(line: &str) -> Vec<String> {
    let mut fields = Vec::with_capacity(5);
    let mut current = String::new();
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => current.push(chars.next().unwrap_or('\\')),
            ':' if fields.len() < 4 => fields.push(std::mem::take(&mut current)),
            _ => current.push(c),
        }
    }
    fields.push(current);
    fields
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_full_url() {
        let config = PgConfig::from_url("postgres://skwad:secret@db.studio.local:6543/skwad_prod").unwrap();
        assert_eq!(config.host, "db.studio.local");
        assert_eq!(config.port, 6543);
        assert_eq!(config.database, "skwad_prod");
        assert_eq!(config.user, "skwad");
        assert_eq!(config.password.as_deref(), Some("secret"));
        assert!(!config.is_local());
    }

    #[test]
    fn falls_back_to_defaults_for_the_parts_a_url_omits() {
        let config = PgConfig::from_url("postgres://localhost").unwrap();
        assert_eq!(config.port, 5432);
        assert_eq!(config.database, "skwad");
        assert!(config.is_local());
    }

    #[test]
    fn the_password_never_reaches_the_library_folder() {
        let config = PgConfig {
            password: Some("secret".into()),
            ..PgConfig::default()
        };
        let raw = serde_json::to_string(&config).unwrap();
        assert!(!raw.contains("secret"), "database.json must not carry a credential");
        assert!(!config.describe().contains("secret"), "describe() is logged");
    }

    /// The three connection failures have nothing in common as problems — a
    /// dead server, a machine with no password, a wrong password — and the pool
    /// reports them almost identically. `DbError` tells them apart by matching
    /// the driver's wording, so this pins all three against a real server: a
    /// driver upgrade that rewords one fails here rather than silently sending
    /// someone to check a firewall that is fine.
    ///
    /// Needs a reachable server, which the test suite already requires.
    #[test]
    fn connection_failures_are_classified_by_cause() {
        let url = std::env::var("SKWAD_TEST_DATABASE_URL")
            .unwrap_or_else(|_| "postgres://postgres:postgres@localhost:5432/skwad_test".to_string());
        let reachable = PgConfig::from_url(&url).expect("valid test url");

        // `Database` is deliberately not `Debug` — it holds a connection pool —
        // so this unwraps by hand rather than widening its API for a test.
        let failure = |config: PgConfig| match crate::Database::connect(config) {
            Ok(_) => panic!("expected the connection to fail"),
            Err(error) => error,
        };

        // --- a server that wants a password it was not given ---------------
        let error = failure(PgConfig {
            password: None,
            ..reachable.clone()
        });
        assert!(
            error.is_missing_credential(),
            "a missing password should be recognised as one, got: {error}"
        );
        assert!(!error.is_unavailable(), "the server answered, so not unavailable");
        assert!(!error.is_rejected(), "it never got as far as being refused");

        // --- a server that answered and refused ----------------------------
        let error = failure(PgConfig {
            password: Some("definitely-not-the-password".into()),
            ..reachable.clone()
        });
        assert!(error.is_rejected(), "a bad password is a rejection, got: {error}");
        assert!(
            !error.is_unavailable(),
            "a rejection means the server is up, got: {error}"
        );
        assert!(!error.is_missing_credential(), "a password was supplied");

        // --- nothing listening ---------------------------------------------
        let error = failure(PgConfig {
            port: 59_999,
            password: Some("anything".into()),
            ..reachable
        });
        assert!(
            error.is_unavailable(),
            "a dead port should read as unavailable: {error}"
        );
        assert!(!error.is_missing_credential());
        assert!(!error.is_rejected());
    }

    #[test]
    fn pgpass_lines_split_on_unescaped_colons_only() {
        assert_eq!(
            split_pgpass_line("localhost:5432:skwad:skwad:secret"),
            vec!["localhost", "5432", "skwad", "skwad", "secret"]
        );
        // A password may contain colons — only the first four separate fields.
        assert_eq!(
            split_pgpass_line("*:*:*:skwad:pa:ss:word").last().unwrap(),
            "pa:ss:word"
        );
        // …and an escaped colon is part of the value, not a separator.
        assert_eq!(
            split_pgpass_line(r"local\:host:5432:skwad:skwad:secret")[0],
            "local:host"
        );
        assert_eq!(split_pgpass_line(r"*:*:*:*:back\\slash").last().unwrap(), r"back\slash");
    }

    #[test]
    fn a_matching_pgpass_entry_supplies_the_password() {
        let dir = tempfile::tempdir().unwrap();
        let pgpass = dir.path().join("pgpass.conf");
        std::fs::write(
            &pgpass,
            "# a comment\n\
             other-host:5432:skwad:skwad:wrong\n\
             localhost:5432:skwad:skwad:right\n\
             *:*:*:*:fallback\n",
        )
        .unwrap();
        std::env::set_var("PGPASSFILE", &pgpass);

        let config = PgConfig::default();
        assert_eq!(config.password_from_pgpass().as_deref(), Some("right"));

        // A database nothing names explicitly falls through to the wildcard.
        let other = PgConfig {
            database: "something_else".into(),
            ..PgConfig::default()
        };
        assert_eq!(other.password_from_pgpass().as_deref(), Some("fallback"));

        std::env::remove_var("PGPASSFILE");
    }

    #[test]
    fn a_saved_config_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let config = PgConfig {
            host: "studio-nas".into(),
            port: 5433,
            database: "bgms".into(),
            user: "editor".into(),
            ..PgConfig::default()
        };
        config.save(dir.path()).unwrap();

        let loaded = PgConfig::resolve(dir.path());
        assert_eq!(loaded.host, "studio-nas");
        assert_eq!(loaded.port, 5433);
        assert_eq!(loaded.database, "bgms");
    }
}

/// Splits a `schema=` query parameter off a connection URL, returning the
/// URL without it. Only plain identifiers are accepted, because the value
/// ends up in `search_path` and `CREATE SCHEMA` unquoted.
fn split_schema(url: &str) -> Result<(String, Option<String>)> {
    let Some((base, query)) = url.split_once('?') else {
        return Ok((url.to_string(), None));
    };
    let mut schema = None;
    let mut kept = Vec::new();
    for pair in query.split('&') {
        match pair.split_once('=') {
            Some(("schema", value)) => {
                if value.is_empty() || !value.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                    return Err(DbError::other(format!(
                        "schema `{value}` is not a plain identifier (letters, digits and underscores)"
                    )));
                }
                schema = Some(value.to_string());
            }
            _ => kept.push(pair),
        }
    }
    let rebuilt = if kept.is_empty() {
        base.to_string()
    } else {
        format!("{base}?{}", kept.join("&"))
    };
    Ok((rebuilt, schema))
}

#[cfg(test)]
mod schema_tests {
    use super::*;

    #[test]
    fn a_schema_parameter_is_ours_and_the_rest_of_the_url_survives() {
        let config = PgConfig::from_url("postgres://u:p@host:5433/db?schema=smoke&sslmode=disable").unwrap();
        assert_eq!(config.schema.as_deref(), Some("smoke"));
        assert_eq!(config.database, "db");
        assert!(config.describe().ends_with("/db/smoke"));

        assert!(PgConfig::from_url("postgres://u:p@host/db?schema=bad-name").is_err());
        assert!(PgConfig::from_url("postgres://u:p@host/db").unwrap().schema.is_none());
    }
}
