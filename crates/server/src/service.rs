//! Running under the Windows Service Control Manager.
//!
//! The SCM is not just "start this exe at boot". A service must connect back
//! to the SCM within about 30 seconds, register a control handler, and then
//! keep reporting its state; a plain console binary registered with
//! `sc create` starts, never reports `Running`, and is killed as unresponsive.
//! `services/backend` solved this for the signing service; this is the same
//! machinery with the server's name on it.
//!
//! The same binary still runs in the foreground. [`try_run_as_service`] asks
//! the SCM to dispatch; error 1063 means "you have no service controller",
//! i.e. a human started it from a shell, and the caller runs normally.

#![cfg(windows)]

use std::ffi::OsString;
use std::sync::mpsc;
use std::time::Duration;

use windows_service::service::{
    ServiceAccess, ServiceControl, ServiceControlAccept, ServiceErrorControl, ServiceExitCode, ServiceInfo,
    ServiceStartType, ServiceState, ServiceStatus, ServiceType,
};
use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};
use windows_service::{define_windows_service, service_dispatcher};

use skwad_server::{cli_overrides, config, ServerConfig};

pub const SERVICE_NAME: &str = "SkwadServer";
pub const DISPLAY_NAME: &str = "SKWAD Media Organiser Server";
const DESCRIPTION: &str = "Owns the SKWAD library: serves clients, runs the processing pipeline and the finishing stages.";
const SERVICE_TYPE: ServiceType = ServiceType::OWN_PROCESS;

define_windows_service!(ffi_service_main, service_main);

fn service_main(_arguments: Vec<OsString>) {
    let _guard = init_file_logging();
    if let Err(error) = run() {
        tracing::error!(%error, "the service stopped with an error");
    }
}

fn init_file_logging() -> Option<tracing_appender::non_blocking::WorkerGuard> {
    let dir = config::default_log_dir();
    std::fs::create_dir_all(&dir).ok()?;
    let appender = tracing_appender::rolling::daily(&dir, "server.log");
    let (writer, guard) = tracing_appender::non_blocking(appender);
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_env("SKWAD_LOG").unwrap_or_else(|_| "info".into()))
        .with_writer(writer)
        .with_ansi(false)
        .try_init()
        .ok()?;
    Some(guard)
}

fn run() -> Result<(), windows_service::Error> {
    let (shutdown_tx, shutdown_rx) = mpsc::channel();
    let handler = move |control| match control {
        ServiceControl::Stop | ServiceControl::Shutdown => {
            let _ = shutdown_tx.send(());
            ServiceControlHandlerResult::NoError
        }
        ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
        _ => ServiceControlHandlerResult::NotImplemented,
    };
    let status_handle = service_control_handler::register(SERVICE_NAME, handler)?;

    let report = |state: ServiceState, controls: ServiceControlAccept, wait_hint: Duration| ServiceStatus {
        service_type: SERVICE_TYPE,
        current_state: state,
        controls_accepted: controls,
        exit_code: ServiceExitCode::Win32(0),
        checkpoint: 0,
        wait_hint,
        process_id: None,
    };

    // Connecting to PostgreSQL and binding the port happen inside `serve`, so
    // claim a start window first rather than letting the SCM time us out.
    status_handle.set_service_status(report(
        ServiceState::StartPending,
        ServiceControlAccept::empty(),
        Duration::from_secs(30),
    ))?;

    let config = match ServerConfig::resolve(Default::default()) {
        Ok(config) => config,
        Err(error) => {
            tracing::error!(%error, "the service configuration is unusable");
            status_handle.set_service_status(ServiceStatus {
                service_type: SERVICE_TYPE,
                current_state: ServiceState::Stopped,
                controls_accepted: ServiceControlAccept::empty(),
                exit_code: ServiceExitCode::ServiceSpecific(2),
                checkpoint: 0,
                wait_hint: Duration::default(),
                process_id: None,
            })?;
            return Ok(());
        }
    };

    let running_handle = status_handle;
    let on_listening = move || {
        let _ = running_handle.set_service_status(ServiceStatus {
            service_type: SERVICE_TYPE,
            current_state: ServiceState::Running,
            controls_accepted: ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN,
            exit_code: ServiceExitCode::Win32(0),
            checkpoint: 0,
            wait_hint: Duration::default(),
            process_id: None,
        });
    };

    let outcome = skwad_server::serve_blocking(config, on_listening, async move {
        let _ = tokio::task::spawn_blocking(move || {
            let _ = shutdown_rx.recv();
        })
        .await;
    });

    let exit_code = match outcome {
        Ok(()) => ServiceExitCode::Win32(0),
        Err(error) => {
            tracing::error!(%error, "the server exited with an error");
            ServiceExitCode::ServiceSpecific(1)
        }
    };

    status_handle.set_service_status(ServiceStatus {
        service_type: SERVICE_TYPE,
        current_state: ServiceState::Stopped,
        controls_accepted: ServiceControlAccept::empty(),
        exit_code,
        checkpoint: 0,
        wait_hint: Duration::default(),
        process_id: None,
    })?;
    Ok(())
}

/// Hands control to the SCM if we were started by it. `Ok(false)` means a
/// person ran this from a shell.
pub fn try_run_as_service() -> Result<bool, windows_service::Error> {
    const NO_SERVICE_CONTROLLER: i32 = 1063;
    match service_dispatcher::start(SERVICE_NAME, ffi_service_main) {
        Ok(()) => Ok(true),
        Err(windows_service::Error::Winapi(error)) if error.raw_os_error() == Some(NO_SERVICE_CONTROLLER) => Ok(false),
        Err(error) => Err(error),
    }
}

/// `install | uninstall | start | stop | status`.
pub fn manage(command: &str, rest: &[String]) -> Result<(), String> {
    match command {
        "install" => install(rest),
        "uninstall" => uninstall(),
        "start" => {
            require_elevation("Starting the service")?;
            start().map_err(|e| format!("could not start the service: {e}"))?;
            println!("started {SERVICE_NAME}");
            Ok(())
        }
        "stop" => {
            require_elevation("Stopping the service")?;
            stop().map_err(|e| format!("could not stop the service: {e}"))?;
            println!("stopped {SERVICE_NAME}");
            Ok(())
        }
        "status" => {
            match status().map_err(|e| format!("could not query the service: {e}"))? {
                Some(state) => println!("{SERVICE_NAME}: {state:?}"),
                None => println!("{SERVICE_NAME}: not installed"),
            }
            Ok(())
        }
        other => Err(format!("unrecognised command: {other}")),
    }
}

fn require_elevation(action: &str) -> Result<(), String> {
    if ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CREATE_SERVICE).is_ok() {
        return Ok(());
    }
    Err(format!(
        "{action} needs an elevated shell.\n\n\
         Open a terminal as Administrator (Win+X → \"Terminal (Admin)\") and run it there."
    ))
}

/// Registers the service and writes the config file it will run from, so the
/// flags given here are what the service uses on every boot.
fn install(rest: &[String]) -> Result<(), String> {
    require_elevation("Installing the service")?;
    let exe = std::env::current_exe().map_err(|e| format!("could not locate this executable: {e}"))?;
    if status().map_err(|e| format!("could not query the service: {e}"))?.is_some() {
        return Err(format!(
            "the {SERVICE_NAME} service is already installed.\n\
             Run `skwad-server uninstall` first if you want to replace it."
        ));
    }

    let overrides = cli_overrides(rest)?;
    let config = ServerConfig::resolve(overrides).map_err(|e| e.to_string())?;
    let config_path = config::default_config_path();
    if let Some(parent) = config_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("could not create {}: {e}", parent.display()))?;
    }
    std::fs::write(&config_path, config.render_env_file())
        .map_err(|e| format!("could not write {}: {e}", config_path.display()))?;
    println!("wrote {}", config_path.display());

    let log_dir = config::default_log_dir();
    std::fs::create_dir_all(&log_dir).map_err(|e| format!("could not create {}: {e}", log_dir.display()))?;
    std::fs::create_dir_all(&config.library_root)
        .map_err(|e| format!("could not create {}: {e}", config.library_root.display()))?;

    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CREATE_SERVICE)
        .map_err(|e| format!("could not open the service manager: {e}"))?;
    let service = manager
        .create_service(
            &ServiceInfo {
                name: SERVICE_NAME.into(),
                display_name: DISPLAY_NAME.into(),
                service_type: SERVICE_TYPE,
                start_type: ServiceStartType::AutoStart,
                error_control: ServiceErrorControl::Normal,
                executable_path: exe.clone(),
                launch_arguments: vec![],
                dependencies: vec![],
                // LocalSystem rather than LocalService: the server reads shoot
                // folders and writes the library, which are ordinary user
                // paths a restricted service account cannot reach. Narrowing
                // this to a dedicated account is the operator's call.
                account_name: None,
                account_password: None,
            },
            ServiceAccess::CHANGE_CONFIG | ServiceAccess::START,
        )
        .map_err(|e| format!("could not register the service: {e}"))?;
    service
        .set_description(DESCRIPTION)
        .map_err(|e| format!("could not describe the service: {e}"))?;

    println!("registered {SERVICE_NAME} ({DISPLAY_NAME})");
    println!("  binary:  {}", exe.display());
    println!("  library: {}", config.library_root.display());
    println!("  listen:  {}{}", config.bind, if config.tls_enabled() { " (TLS)" } else { "" });
    println!("  logs:    {}", log_dir.display());
    println!("\nStart it with:  skwad-server start");
    Ok(())
}

fn uninstall() -> Result<(), String> {
    require_elevation("Uninstalling the service")?;
    if status().map_err(|e| format!("could not query the service: {e}"))?.is_none() {
        println!("the {SERVICE_NAME} service is not installed");
        return Ok(());
    }
    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)
        .map_err(|e| format!("could not open the service manager: {e}"))?;
    let service = manager
        .open_service(
            SERVICE_NAME,
            ServiceAccess::QUERY_STATUS | ServiceAccess::STOP | ServiceAccess::DELETE,
        )
        .map_err(|e| format!("could not open the service: {e}"))?;
    if service.query_status().map_err(|e| e.to_string())?.current_state != ServiceState::Stopped {
        let _ = service.stop();
        for _ in 0..30 {
            std::thread::sleep(Duration::from_millis(500));
            if service.query_status().map_err(|e| e.to_string())?.current_state == ServiceState::Stopped {
                break;
            }
        }
    }
    service.delete().map_err(|e| format!("could not remove the service: {e}"))?;
    println!("removed {SERVICE_NAME}; the library and its configuration were left in place");
    Ok(())
}

fn start() -> Result<(), windows_service::Error> {
    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)?;
    let service = manager.open_service(SERVICE_NAME, ServiceAccess::START)?;
    service.start::<&str>(&[])
}

fn stop() -> Result<(), windows_service::Error> {
    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)?;
    let service = manager.open_service(SERVICE_NAME, ServiceAccess::STOP)?;
    service.stop().map(|_| ())
}

fn status() -> Result<Option<ServiceState>, windows_service::Error> {
    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)?;
    match manager.open_service(SERVICE_NAME, ServiceAccess::QUERY_STATUS) {
        Ok(service) => Ok(Some(service.query_status()?.current_state)),
        // 1060: not installed, which is an answer rather than a failure.
        Err(windows_service::Error::Winapi(error)) if error.raw_os_error() == Some(1060) => Ok(None),
        Err(error) => Err(error),
    }
}
