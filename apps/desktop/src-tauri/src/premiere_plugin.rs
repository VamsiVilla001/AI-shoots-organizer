//! Installs the Premiere Pro panel on the user's machine, so nobody has to.
//!
//! The panel is a UXP plugin, and UXP plugins are not loadable by copying a
//! folder anywhere: Premiere reads a registry of installed plugins that only
//! Adobe's own installer agent writes correctly. The two documented ways in
//! are the UXP Developer Tool (a developer machine only) and that agent —
//! `UnifiedPluginInstallerAgent`, which ships with the Creative Cloud desktop
//! app and is therefore already present on every machine that runs Premiere.
//! This module drives the agent.
//!
//! It runs from the app rather than from the installer on purpose. UXP plugins
//! install per-user, into the running user's profile; the Windows bundle is
//! `perMachine`, so an NSIS hook would run elevated and install the panel into
//! whichever account happened to run the installer — often an administrator
//! who never opens Premiere. Doing it at launch puts it in the right profile
//! by construction, and gets macOS for free instead of needing a separate
//! `.pkg` postinstall script.
//!
//! Nothing here can fail the app. A machine with no Creative Cloud, no
//! Premiere, or an agent that has moved is a perfectly ordinary machine to run
//! SKWAD on — every path logs and returns instead of propagating. In
//! particular this module is entirely independent of `premiere_api`: the
//! bridge serves whatever panel is installed, however it got there, including
//! one loaded by hand through the UXP Developer Tool.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Serialize;
use tauri::{AppHandle, Manager};

/// Must match `id` in apps/premiere-panel/manifest.json.
const PLUGIN_ID: &str = "com.skwad.premiere-panel";

/// Where `scripts/package-premiere-panel.mjs` stages the package, relative to
/// the bundle's resource directory.
const BUNDLED_CCX: &str = "resources/premiere-panel/skwad-collections.ccx";

/// The panel's manifest, staged beside the package by the same script.
///
/// The version is read from here rather than taken to be the app's own,
/// because the panel carries its own version and is under no obligation to
/// match: comparing `CARGO_PKG_VERSION` against an installed panel's manifest
/// would disagree permanently and reinstall on every single launch.
const BUNDLED_MANIFEST: &str = "resources/premiere-panel/manifest.json";

/// What the panel install looks like right now — surfaced to Settings so a
/// failed or skipped install is something the user can see and retry, rather
/// than a panel that silently never appears in Premiere.
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PanelStatus {
    /// The version this build of the app ships, if it was packaged at all.
    pub bundled_version: Option<String>,
    /// The version currently installed for this user, if any.
    pub installed_version: Option<String>,
    /// Whether Creative Cloud's installer agent could be found.
    pub installer_available: bool,
}

/// Adobe's installer agent, which ships inside the Creative Cloud desktop app.
fn installer_agent() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        // 32-bit common files on a 64-bit Windows, which is where the Creative
        // Cloud desktop app puts its shared components; the plain variable is
        // the fallback for the 32-bit case.
        let roots = [
            std::env::var_os("CommonProgramFiles(x86)"),
            std::env::var_os("CommonProgramFiles"),
        ];
        for root in roots.into_iter().flatten() {
            let candidate = PathBuf::from(root)
                .join("Adobe/Adobe Desktop Common/RemoteComponents/UPI")
                .join("UnifiedPluginInstallerAgent/UnifiedPluginInstallerAgent.exe");
            if candidate.is_file() {
                return Some(candidate);
            }
        }
        None
    }
    #[cfg(target_os = "macos")]
    {
        let candidate = PathBuf::from(
            "/Library/Application Support/Adobe/Adobe Desktop Common/RemoteComponents/UPI\
             /UnifiedPluginInstallerAgent/UnifiedPluginInstallerAgent.app/Contents/MacOS\
             /UnifiedPluginInstallerAgent",
        );
        candidate.is_file().then_some(candidate)
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        None
    }
}

/// Where the agent installs plugins for the current user. Read rather than
/// written — the install itself always goes through the agent, because this
/// directory is only half the story (Premiere also needs the registry entry
/// beside it, which is exactly what hand-copying a plugin folder fails to do).
fn installed_plugins_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        std::env::var_os("APPDATA").map(|a| PathBuf::from(a).join("Adobe/UXP/Plugins/External"))
    }
    #[cfg(target_os = "macos")]
    {
        std::env::var_os("HOME")
            .map(|h| PathBuf::from(h).join("Library/Application Support/Adobe/UXP/Plugins/External"))
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        None
    }
}

/// The version of the plugin installed in `dir`, if that is *our* plugin.
///
/// `None` covers every uninteresting case alike — no manifest, unreadable,
/// unparseable, or somebody else's plugin — because the caller's next move is
/// the same for all of them: look at the next directory.
fn our_version_in(dir: &Path) -> Option<String> {
    let text = std::fs::read_to_string(dir.join("manifest.json")).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    if value.get("id").and_then(|id| id.as_str()) != Some(PLUGIN_ID) {
        return None;
    }
    value.get("version")?.as_str().map(str::to_owned)
}

/// The installed version of our panel, by reading the manifests under the
/// external-plugins directory.
///
/// The agent names those folders itself and has changed the convention across
/// Creative Cloud releases (plain id, id with a version suffix, id with a
/// hash), so this matches on the `id` *inside* each manifest rather than on
/// the folder name — the one part Adobe cannot rename out from under us.
fn installed_version() -> Option<String> {
    let dir = installed_plugins_dir()?;
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .find_map(|entry| our_version_in(&entry.path()))
}

/// The `.ccx` this build ships, if `scripts/package-premiere-panel.mjs` ran
/// before the bundle was built.
fn bundled_ccx(app: &AppHandle) -> Option<PathBuf> {
    let path = app
        .path()
        .resolve(BUNDLED_CCX, tauri::path::BaseDirectory::Resource)
        .ok()?;
    path.is_file().then_some(path)
}

/// The panel version this build ships, from the staged manifest.
fn bundled_version(app: &AppHandle) -> Option<String> {
    let manifest = app
        .path()
        .resolve(BUNDLED_MANIFEST, tauri::path::BaseDirectory::Resource)
        .ok()?;
    our_version_in(manifest.parent()?)
}

fn run_agent(agent: &Path, ccx: &Path) -> std::io::Result<std::process::Output> {
    let mut command = Command::new(agent);
    command.arg("--install").arg(ccx);
    // Without this the agent flashes a console window on every launch.
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command.output()
}

/// Installs the bundled panel, or upgrades an older one.
pub fn install(app: &AppHandle) -> Result<Option<String>, String> {
    let Some(ccx) = bundled_ccx(app) else {
        return Err(
            "this build does not ship the Premiere panel — run `npm run package:premiere-panel` before building".into(),
        );
    };
    let Some(agent) = installer_agent() else {
        return Err("could not find Adobe's plugin installer — is the Creative Cloud desktop app installed?".into());
    };

    let output = run_agent(&agent, &ccx).map_err(|e| format!("could not run Adobe's plugin installer: {e}"))?;
    if !output.status.success() {
        // The agent writes its reasons to stdout as often as to stderr.
        let stderr = String::from_utf8_lossy(&output.stderr);
        let detail = if stderr.trim().is_empty() {
            String::from_utf8_lossy(&output.stdout).trim().to_owned()
        } else {
            stderr.trim().to_owned()
        };
        return Err(format!(
            "Adobe's plugin installer refused the panel{}",
            if detail.is_empty() {
                String::new()
            } else {
                format!(": {detail}")
            }
        ));
    }

    let now = installed_version();
    tracing::info!(installed = ?now, "installed the Premiere panel");
    Ok(now)
}

/// Installs the panel on launch if it is missing or out of date.
///
/// Runs on its own thread and swallows every failure: the panel is a
/// convenience, and a machine without Premiere must still start normally.
pub fn ensure_installed(app: AppHandle) {
    std::thread::spawn(move || {
        let Some(ccx) = bundled_ccx(&app) else {
            tracing::debug!("no Premiere panel in this build; skipping the install");
            return;
        };
        if installer_agent().is_none() {
            tracing::debug!("no Adobe plugin installer on this machine; skipping the install");
            return;
        }

        // A version change in the panel's own manifest is the signal to
        // reinstall. If the manifest is missing the install still runs — the
        // agent is idempotent, so the cost of being wrong here is one wasted
        // call, not a broken panel.
        let bundled = bundled_version(&app);
        let installed = installed_version();
        if installed.is_some() && installed == bundled {
            tracing::debug!(version = ?bundled, "the Premiere panel is already current");
            return;
        }

        tracing::info!(
            from = ?installed,
            to = ?bundled,
            package = %ccx.display(),
            "installing the Premiere panel",
        );
        if let Err(e) = install(&app) {
            // Not an error for the app: Premiere may not be installed, or
            // Creative Cloud may want a signed package. Settings shows the
            // same state and offers a retry.
            tracing::warn!(error = %e, "could not install the Premiere panel automatically");
        }
    });
}

/// What Settings shows: whether the panel is installed, and whether it could be.
#[tauri::command]
pub fn premiere_panel_status(app: AppHandle) -> PanelStatus {
    PanelStatus {
        bundled_version: bundled_ccx(&app).and_then(|_| bundled_version(&app)),
        installed_version: installed_version(),
        installer_available: installer_agent().is_some(),
    }
}

/// Settings' "Install panel" button — the manual path for a machine where the
/// automatic install was skipped or refused.
#[tauri::command]
pub fn install_premiere_panel(app: AppHandle) -> Result<PanelStatus, String> {
    install(&app)?;
    Ok(premiere_panel_status(app))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugin_id_matches_the_panel_manifest() {
        // The panel's manifest is the source of truth for the id this module
        // looks for; a rename there must not silently orphan the install check.
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../premiere-panel/manifest.json");
        let text = std::fs::read_to_string(&manifest).expect("the panel manifest should be readable");
        let value: serde_json::Value = serde_json::from_str(&text).expect("it should be valid JSON");
        assert_eq!(value["id"].as_str(), Some(PLUGIN_ID));
    }

    /// Writes a plugin directory the way the installer agent lays one out.
    fn plugin_dir(manifest: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("a temp dir");
        std::fs::write(dir.path().join("manifest.json"), manifest).expect("writable");
        dir
    }

    #[test]
    fn reads_the_version_of_our_own_plugin() {
        let dir = plugin_dir(&format!(r#"{{"id":"{PLUGIN_ID}","version":"2.1.0"}}"#));
        assert_eq!(our_version_in(dir.path()).as_deref(), Some("2.1.0"));
    }

    #[test]
    fn ignores_somebody_elses_plugin() {
        // The external-plugins directory holds every UXP plugin the user has,
        // so matching the id is what keeps us from reporting another vendor's
        // version as our own.
        let dir = plugin_dir(r#"{"id":"com.example.other","version":"9.9.9"}"#);
        assert_eq!(our_version_in(dir.path()), None);
    }

    #[test]
    fn tolerates_a_damaged_manifest() {
        let dir = plugin_dir("not json");
        assert_eq!(our_version_in(dir.path()), None);
    }

    #[test]
    fn tolerates_a_directory_with_no_manifest() {
        let dir = tempfile::tempdir().expect("a temp dir");
        assert_eq!(our_version_in(dir.path()), None);
    }
}
