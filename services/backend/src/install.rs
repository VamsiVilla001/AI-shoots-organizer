//! `install` / `uninstall` / `start` / `status`.
//!
//! Registering a service, generating the organisation signing key and locking
//! down the file that holds it are one operation as far as an operator is
//! concerned, so they are one command. Doing it by hand means `sc create`, a
//! separate keygen, a hand-written config file and an `icacls` invocation, and
//! forgetting the last of those leaves the signing key world-readable.

#![cfg(windows)]

use std::path::Path;
use std::process::Command;

use crate::{config, service};

pub fn dispatch(command: &str) -> Result<(), String> {
    match command {
        "install" => install(),
        "uninstall" => uninstall(),
        "start" => start(),
        "status" => status(),
        other => Err(format!("unrecognised command: {other}")),
    }
}

fn require_elevation(action: &str) -> Result<(), String> {
    if is_elevated() {
        return Ok(());
    }
    Err(format!(
        "{action} needs an elevated shell.\n\n\
         Open a terminal as Administrator (Win+X → \"Terminal (Admin)\") and run it there."
    ))
}

/// Whether this process can manage services.
///
/// Rather than inspecting the token, this asks the Service Control Manager for
/// the access the operation actually needs — which is the thing we care about,
/// and stays correct under UAC configurations where an admin token is filtered.
fn is_elevated() -> bool {
    use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};
    ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CREATE_SERVICE).is_ok()
}

fn install() -> Result<(), String> {
    require_elevation("Installing the service")?;

    let exe = std::env::current_exe().map_err(|e| format!("could not locate this executable: {e}"))?;
    if is_inside_target_dir(&exe) {
        eprintln!(
            "note: installing from {}\n      \
             The service will run this exact file, so a `cargo clean` or a rebuild\n      \
             will stop it working. For anything but a test, copy the release binary\n      \
             somewhere stable first.\n",
            exe.display()
        );
    }

    if service::status()
        .map_err(|e| format!("could not query the service: {e}"))?
        .is_some()
    {
        return Err(format!(
            "the {} service is already installed.\n\
             Run `skwad-backend uninstall` first if you want to replace it.",
            service::SERVICE_NAME
        ));
    }

    // --- secrets ----------------------------------------------------------
    // Never overwrite an existing config: it holds the organisation signing
    // key, and replacing it would silently invalidate every catalogue already
    // published under the old one. Reusing it is also the correct behaviour
    // when reinstalling the service after an upgrade.
    let config_path = config::default_config_path();
    if config_path.exists() {
        println!(
            "keeping the existing secrets at {}\n  \
             (delete that file yourself if you really mean to rotate the signing key)",
            config_path.display()
        );
    } else {
        let parent = config_path
            .parent()
            .ok_or_else(|| format!("{} has no parent directory", config_path.display()))?;
        std::fs::create_dir_all(parent).map_err(|e| format!("could not create {}: {e}", parent.display()))?;
        let rendered = config::render(&crate::generate_secrets());
        std::fs::write(&config_path, rendered)
            .map_err(|e| format!("could not write {}: {e}", config_path.display()))?;
        println!("generated a signing key and wrote {}", config_path.display());
    }
    restrict_to_service_account(&config_path)?;

    // --- logs -------------------------------------------------------------
    let log_dir = config::default_log_dir();
    std::fs::create_dir_all(&log_dir).map_err(|e| format!("could not create {}: {e}", log_dir.display()))?;
    grant_service_account_write(&log_dir)?;

    // --- the service itself -----------------------------------------------
    service::install(&exe).map_err(|e| format!("could not register the service: {e}"))?;
    println!("registered {} ({})", service::SERVICE_NAME, service::DISPLAY_NAME);
    println!("  binary:  {}", exe.display());
    println!("  account: NT AUTHORITY\\LocalService");
    println!("  startup: automatic");
    println!("  logs:    {}", log_dir.display());
    println!("\nStart it with:  skwad-backend start");
    Ok(())
}

fn uninstall() -> Result<(), String> {
    require_elevation("Uninstalling the service")?;
    if service::status()
        .map_err(|e| format!("could not query the service: {e}"))?
        .is_none()
    {
        println!("the {} service is not installed", service::SERVICE_NAME);
        return Ok(());
    }
    service::uninstall().map_err(|e| format!("could not remove the service: {e}"))?;
    println!("removed {}", service::SERVICE_NAME);
    println!(
        "\nThe signing key at {} was left in place — delete it deliberately if you\n\
         are decommissioning this machine.",
        config::default_config_path().display()
    );
    Ok(())
}

fn start() -> Result<(), String> {
    require_elevation("Starting the service")?;
    service::start().map_err(|e| format!("could not start the service: {e}"))?;
    println!("started {}", service::SERVICE_NAME);
    println!("Check it with:  curl http://127.0.0.1:8787/health");
    Ok(())
}

fn status() -> Result<(), String> {
    match service::status().map_err(|e| format!("could not query the service: {e}"))? {
        None => println!("{}: not installed", service::SERVICE_NAME),
        Some(state) => println!("{}: {state:?}", service::SERVICE_NAME),
    }
    // Report which secrets resolve, not just whether the file exists: a
    // half-filled config starts the service and then fails at `load`, which
    // shows up as a service that will not stay running and nothing else.
    let config_path = config::default_config_path();
    let source = config::Source::discover();
    let missing: Vec<&str> = config::REQUIRED_VARS
        .iter()
        .copied()
        .filter(|name| source.get(name).is_none())
        .collect();

    println!(
        "secrets: {}{}",
        config_path.display(),
        if config_path.exists() { "" } else { " (missing)" }
    );
    if missing.is_empty() {
        println!("         all {} required values resolve", config::REQUIRED_VARS.len());
    } else {
        println!("         NOT SET: {}", missing.join(", "));
    }
    println!("logs:    {}", config::default_log_dir().display());
    Ok(())
}

/// True when the path is a cargo build output, which is not a stable home for
/// a service binary.
fn is_inside_target_dir(exe: &Path) -> bool {
    exe.components()
        .any(|component| component.as_os_str().eq_ignore_ascii_case("target"))
}

/// Removes inherited permissions and grants read to SYSTEM, Administrators and
/// the service account only.
///
/// `%PROGRAMDATA%` grants Users write by default through inheritance, so
/// without this the signing key would be readable — and replaceable — by any
/// account on the machine.
fn restrict_to_service_account(path: &Path) -> Result<(), String> {
    icacls(path, &["/inheritance:r"])?;
    icacls(path, &["/grant:r", "*S-1-5-18:(R)"])?; // NT AUTHORITY\SYSTEM
    icacls(path, &["/grant:r", "*S-1-5-32-544:(F)"])?; // BUILTIN\Administrators
    icacls(path, &["/grant:r", "*S-1-5-19:(R)"])?; // NT AUTHORITY\LocalService
    println!("locked down {} (SYSTEM, Administrators, LocalService)", path.display());
    Ok(())
}

fn grant_service_account_write(dir: &Path) -> Result<(), String> {
    // Inherited by files created later, so each day's rolled log is writable.
    icacls(dir, &["/grant", "*S-1-5-19:(OI)(CI)(M)"])?;
    Ok(())
}

/// Well-known SIDs rather than names, because the display names are localised —
/// `icacls /grant "LocalService"` fails on a non-English Windows.
fn icacls(path: &Path, args: &[&str]) -> Result<(), String> {
    let output = Command::new("icacls")
        .arg(path)
        .args(args)
        .output()
        .map_err(|e| format!("could not run icacls: {e}"))?;
    if output.status.success() {
        return Ok(());
    }
    Err(format!(
        "icacls {} {} failed: {}{}",
        path.display(),
        args.join(" "),
        String::from_utf8_lossy(&output.stdout).trim(),
        String::from_utf8_lossy(&output.stderr).trim()
    ))
}
