//! `GET /health` — the diagnostics a headless box has nowhere else to show.
//!
//! The same checks back `skwad-server doctor` and the startup log, so a
//! support question is answered by looking rather than guessing. Anyone may
//! ask whether the server is up; the details behind each check — paths,
//! hashes, counts — are only shown to a signed-in caller.

use std::sync::Arc;

use axum::extract::State;
use axum::Extension;
use axum::Json;
use serde::Serialize;
use skwad_app_core::models::ModelRegistry;
use skwad_app_core::AppState;
use skwad_database::Db;

use crate::auth::SessionRecord;
use crate::error::{blocking, ApiResult};
use crate::state::ServerState;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Check {
    pub name: &'static str,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Health {
    /// `ok` when every check passes, `degraded` otherwise.
    pub status: &'static str,
    pub version: &'static str,
    pub api_version: u32,
    pub uptime_seconds: u64,
    pub checks: Vec<Check>,
}

/// Runs every check against the core. Details are always computed; the
/// route decides whether to show them.
pub fn run_checks(core: &AppState) -> Vec<Check> {
    let mut checks = Vec::new();

    // Database: a round trip, and the migration level this build expects.
    let database = core.db.conn().and_then(|mut conn| {
        conn.row_one("SELECT 1", skwad_database::params![])?;
        skwad_database::migrations::current_version(&mut conn)
    });
    let expected = skwad_database::migrations::target_version();
    checks.push(match database {
        Ok(version) if version == expected => Check {
            name: "database",
            ok: true,
            detail: Some(format!("{} at schema version {version}", core.db.describe())),
        },
        Ok(version) => Check {
            name: "database",
            ok: false,
            detail: Some(format!("schema version {version}, this build expects {expected}")),
        },
        Err(e) => Check {
            name: "database",
            ok: false,
            detail: Some(e.to_string()),
        },
    });

    // Library writable: the server is the only writer, so this is the one
    // that matters.
    let probe = core.paths.root.join(format!(".write-probe-{}", std::process::id()));
    let writable = std::fs::write(&probe, b"probe").and_then(|_| std::fs::remove_file(&probe));
    checks.push(Check {
        name: "library",
        ok: writable.is_ok(),
        detail: Some(match writable {
            Ok(()) => format!("{} is writable", core.paths.root.display()),
            Err(e) => format!("{} is not writable: {e}", core.paths.root.display()),
        }),
    });

    // Models: both roles resolve, and their identities.
    let settings = core.settings();
    let status = ModelRegistry::new(&core.paths.models)
        .status(settings.detector_model.as_deref(), settings.embedder_model.as_deref());
    checks.push(Check {
        name: "models",
        ok: status.ready,
        detail: Some(if status.ready {
            format!(
                "detector {} embedder {}",
                status.detector_hash.as_deref().map(|h| &h[..12]).unwrap_or("?"),
                status.embedder_hash.as_deref().map(|h| &h[..12]).unwrap_or("?")
            )
        } else {
            status.message
        }),
    });

    // Workers: what is running right now.
    let running = core.db.conn().and_then(|mut conn| {
        let row = conn.row_one(
            "SELECT COUNT(*) FILTER (WHERE state = 'running'), COUNT(*) FILTER (WHERE state = 'queued') FROM jobs",
            skwad_database::params![],
        )?;
        Ok((row.get::<_, i64>(0), row.get::<_, i64>(1)))
    });
    checks.push(match running {
        Ok((running, queued)) => Check {
            name: "workers",
            ok: true,
            detail: Some(format!("{running} running, {queued} queued, {} AI slots", settings.ai_workers)),
        },
        Err(e) => Check {
            name: "workers",
            ok: false,
            detail: Some(e.to_string()),
        },
    });

    checks
}

pub fn summarise(core: &AppState, uptime_seconds: u64, with_detail: bool) -> Health {
    let mut checks = run_checks(core);
    if !with_detail {
        for check in &mut checks {
            check.detail = None;
        }
    }
    Health {
        status: if checks.iter().all(|c| c.ok) { "ok" } else { "degraded" },
        version: env!("CARGO_PKG_VERSION"),
        api_version: crate::config::API_VERSION,
        uptime_seconds,
        checks,
    }
}

pub async fn health(
    State(state): State<Arc<ServerState>>,
    Extension(session): Extension<Option<SessionRecord>>,
) -> ApiResult<Json<Health>> {
    let core = Arc::clone(&state.core);
    let uptime = state.started_at.elapsed().as_secs();
    let with_detail = session.is_some();
    let health = blocking(move || Ok(summarise(&core, uptime, with_detail))).await?;
    Ok(Json(health))
}

/// The same report as text, for the `doctor` command and the startup log.
pub fn render_text(health: &Health) -> String {
    let mut out = format!(
        "skwad-server {} (api {}) — {}\n",
        health.version, health.api_version, health.status
    );
    for check in &health.checks {
        out.push_str(&format!(
            "  [{}] {:<9} {}\n",
            if check.ok { "ok" } else { "!!" },
            check.name,
            check.detail.as_deref().unwrap_or("")
        ));
    }
    out
}
