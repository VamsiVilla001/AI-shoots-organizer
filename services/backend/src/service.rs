//! Running under the Windows Service Control Manager.
//!
//! The SCM is not just "start this exe at boot". A service must connect back to
//! the SCM within about 30 seconds, register a control handler, and then keep
//! reporting its state; a plain console binary registered with `sc create`
//! starts, never reports `Running`, and is killed with "the service did not
//! respond to the start or control request in a timely fashion". That is why
//! this exists rather than a Task Scheduler entry.
//!
//! The same binary still runs in the foreground. [`try_run_as_service`] asks
//! the SCM to dispatch; error 1063 means "you have no service controller", i.e.
//! a human started it from a shell, and the caller falls back to running
//! normally. Nothing about developing against it changes.

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

pub const SERVICE_NAME: &str = "SkwadBackend";
pub const DISPLAY_NAME: &str = "SKWAD Catalogue Backend";
const DESCRIPTION: &str = "Signs and rewraps SKWAD catalogue packages. Holds the organisation signing key; \
                           listens on loopback only.";

/// Services are always reported as their own process.
const SERVICE_TYPE: ServiceType = ServiceType::OWN_PROCESS;

// Generates the `extern "system"` entry point the SCM calls. It hands us no
// useful arguments — everything comes from the config file — but the signature
// is fixed.
define_windows_service!(ffi_service_main, service_main);

fn service_main(_arguments: Vec<OsString>) {
    // Before anything else: a service has no stdout, so without this every
    // diagnostic below would go nowhere and a failure to start would show up
    // only as an error code in services.msc.
    let _guard = init_file_logging();
    if let Err(error) = run() {
        tracing::error!(%error, "the service stopped with an error");
    }
}

/// Sends tracing output to a daily-rolled file under the log directory.
///
/// Returns the appender guard — dropping it stops the writer thread, so it has
/// to outlive the server, which is why `service_main` holds it.
fn init_file_logging() -> Option<tracing_appender::non_blocking::WorkerGuard> {
    let dir = crate::config::default_log_dir();
    std::fs::create_dir_all(&dir).ok()?;
    let appender = tracing_appender::rolling::daily(&dir, "backend.log");
    let (writer, guard) = tracing_appender::non_blocking(appender);
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .with_writer(writer)
        .with_ansi(false)
        .try_init()
        .ok()?;
    Some(guard)
}

fn run() -> Result<(), windows_service::Error> {
    // `Stop` and the machine shutting down both mean the same thing to us.
    // Anything else is declined, which is what tells the SCM not to offer
    // pause/continue in services.msc.
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

    // Binding the port and reading the config happen inside `serve`, so claim a
    // start window first rather than letting the SCM time us out.
    status_handle.set_service_status(report(
        ServiceState::StartPending,
        ServiceControlAccept::empty(),
        Duration::from_secs(20),
    ))?;

    // Reported from inside `serve_blocking`, the moment the socket is actually
    // accepting. Reporting `Running` any earlier would tell the SCM the service
    // is up while the port is still closed, so a dependent service could start
    // against nothing.
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

    // `recv` blocks, so it runs on a blocking thread rather than stalling a
    // runtime worker for the service's whole lifetime.
    let outcome = crate::serve_blocking(on_listening, async move {
        let _ = tokio::task::spawn_blocking(move || {
            let _ = shutdown_rx.recv();
        })
        .await;
    });

    let exit_code = match outcome {
        Ok(()) => ServiceExitCode::Win32(0),
        Err(error) => {
            tracing::error!(%error, "the backend exited with an error");
            // A non-zero code is what makes the SCM's recovery actions fire.
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

/// Hands control to the SCM if we were started by it.
///
/// `Ok(false)` means no service controller was listening — a person ran this
/// from a shell — and the caller should run in the foreground instead.
pub fn try_run_as_service() -> Result<bool, windows_service::Error> {
    const NO_SERVICE_CONTROLLER: i32 = 1063;
    match service_dispatcher::start(SERVICE_NAME, ffi_service_main) {
        Ok(()) => Ok(true),
        Err(windows_service::Error::Winapi(error)) if error.raw_os_error() == Some(NO_SERVICE_CONTROLLER) => Ok(false),
        Err(error) => Err(error),
    }
}

/// Registers the service. Requires an elevated process.
///
/// Runs as `NT AUTHORITY\LocalService`: the service reads one config file,
/// writes a log and listens on loopback, so it needs none of the machine-wide
/// authority `LocalSystem` would hand it.
pub fn install(exe: &std::path::Path) -> Result<(), windows_service::Error> {
    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CREATE_SERVICE)?;
    let service = manager.create_service(
        &ServiceInfo {
            name: SERVICE_NAME.into(),
            display_name: DISPLAY_NAME.into(),
            service_type: SERVICE_TYPE,
            start_type: ServiceStartType::AutoStart,
            error_control: ServiceErrorControl::Normal,
            executable_path: exe.to_path_buf(),
            launch_arguments: vec![],
            dependencies: vec![],
            account_name: Some(r"NT AUTHORITY\LocalService".into()),
            account_password: None,
        },
        ServiceAccess::CHANGE_CONFIG | ServiceAccess::START,
    )?;
    service.set_description(DESCRIPTION)?;
    Ok(())
}

/// Stops the service if it is running, then deregisters it.
pub fn uninstall() -> Result<(), windows_service::Error> {
    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)?;
    let service = manager.open_service(
        SERVICE_NAME,
        ServiceAccess::QUERY_STATUS | ServiceAccess::STOP | ServiceAccess::DELETE,
    )?;

    if service.query_status()?.current_state != ServiceState::Stopped {
        service.stop()?;
        // Deleting while it is still stopping leaves the service "marked for
        // deletion" until every handle closes, which looks like the uninstall
        // silently failed.
        for _ in 0..30 {
            std::thread::sleep(Duration::from_millis(500));
            if service.query_status()?.current_state == ServiceState::Stopped {
                break;
            }
        }
    }

    service.delete()
}

/// Starts an installed service.
pub fn start() -> Result<(), windows_service::Error> {
    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)?;
    let service = manager.open_service(SERVICE_NAME, ServiceAccess::START)?;
    service.start::<&str>(&[])
}

/// Whether the service is registered, and what it is doing.
pub fn status() -> Result<Option<ServiceState>, windows_service::Error> {
    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)?;
    match manager.open_service(SERVICE_NAME, ServiceAccess::QUERY_STATUS) {
        Ok(service) => Ok(Some(service.query_status()?.current_state)),
        // 1060: the service is not installed, which is an answer rather than a
        // failure.
        Err(windows_service::Error::Winapi(error)) if error.raw_os_error() == Some(1060) => Ok(None),
        Err(error) => Err(error),
    }
}
