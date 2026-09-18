//! `skwad-server` — run, diagnose, install.
//!
//! ```text
//! skwad-server [run] [--bind …] [--library …] [--media-roots …] [--tls-cert … --tls-key …]
//! skwad-server doctor            check the database, library, models and workers, then exit
//! skwad-server create-user --email … --name … [--password …] [--admin]
//! skwad-server install …         register the Windows service with these settings (elevated)
//! skwad-server uninstall | start | stop | status
//! skwad-server enrol --server URL --email … --password … --name …   enrol this box as a worker
//! skwad-server worker --server URL --token …                    analyse for a server, headless
//! ```
//!
//! Configuration precedence is command line, then environment, then the
//! config file `install` wrote. A service is started by the Service Control
//! Manager with no environment, so the file is what it runs from.

use std::sync::Arc;

use skwad_server::{cli_overrides, config, ServerConfig};

#[cfg(windows)]
mod service;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (command, rest) = match args.first().map(String::as_str) {
        Some(c @ ("run" | "doctor" | "create-user" | "enrol" | "worker" | "install" | "uninstall" | "start" | "stop" | "status" | "help" | "--help" | "-h")) => (c, &args[1..]),
        Some(flag) if flag.starts_with("--") => ("run", &args[..]),
        Some(other) => {
            eprintln!("unrecognised command: {other}\n");
            print_usage();
            std::process::exit(2);
        }
        None => ("run", &args[..]),
    };

    match command {
        "help" | "--help" | "-h" => print_usage(),
        "doctor" => {
            init_console_logging();
            let config = resolve_or_exit(rest);
            match skwad_server::boot(config) {
                Ok((state, workers)) => {
                    let health = skwad_server::health::summarise(&state.core, 0, true);
                    print!("{}", skwad_server::health::render_text(&health));
                    state.core.begin_shutdown();
                    workers.join();
                    if health.status != "ok" {
                        std::process::exit(1);
                    }
                }
                Err(error) => {
                    eprintln!("skwad-server: {error:#}");
                    std::process::exit(1);
                }
            }
        }
        "create-user" => {
            init_console_logging();
            if let Err(error) = create_user(rest) {
                eprintln!("skwad-server: {error}");
                std::process::exit(1);
            }
        }
        "enrol" => {
            init_console_logging();
            if let Err(error) = enrol(rest) {
                eprintln!("skwad-server: {error}");
                std::process::exit(1);
            }
        }
        "worker" => {
            init_console_logging();
            if let Err(error) = worker(rest) {
                eprintln!("skwad-server: {error}");
                std::process::exit(1);
            }
        }
        #[cfg(windows)]
        "install" | "uninstall" | "start" | "stop" | "status" => {
            init_console_logging();
            if let Err(error) = service::manage(command, rest) {
                eprintln!("\n{error}");
                std::process::exit(1);
            }
        }
        #[cfg(not(windows))]
        "install" | "uninstall" | "start" | "stop" | "status" => {
            eprintln!("`{command}` is Windows-only; use systemd or launchd to supervise this binary.");
            std::process::exit(2);
        }
        _ => run_foreground_or_service(rest),
    }
}

fn resolve_or_exit(rest: &[String]) -> ServerConfig {
    let overrides = match cli_overrides(rest) {
        Ok(overrides) => overrides,
        Err(error) => {
            eprintln!("skwad-server: {error}\n");
            print_usage();
            std::process::exit(2);
        }
    };
    match ServerConfig::resolve(overrides) {
        Ok(config) => config,
        Err(error) => {
            eprintln!("skwad-server: {error}");
            std::process::exit(2);
        }
    }
}

fn run_foreground_or_service(rest: &[String]) {
    // Ask the Service Control Manager to dispatch. It declines when nothing
    // started us as a service, which is how "run from a shell" is told apart
    // without a flag the SCM would have to be told to pass.
    #[cfg(windows)]
    {
        match service::try_run_as_service() {
            Ok(true) => return,
            Ok(false) => {}
            Err(error) => {
                init_console_logging();
                eprintln!("could not talk to the service control manager: {error}");
                std::process::exit(1);
            }
        }
    }

    init_console_logging();
    let config = resolve_or_exit(rest);
    let result = skwad_server::serve_blocking(config, || {}, async {
        let _ = tokio::signal::ctrl_c().await;
    });
    if let Err(error) = result {
        eprintln!("skwad-server: {error:#}");
        std::process::exit(2);
    }
}

/// `create-user --email … --name … [--password …] [--admin]` writes straight
/// to the credential file, so a headless box gets its first administrator
/// without a window.
fn create_user(rest: &[String]) -> Result<(), String> {
    let mut email = None;
    let mut name = None;
    let mut password = None;
    let mut admin = false;
    let mut config_args = Vec::new();
    let mut i = 0;
    while i < rest.len() {
        match rest[i].as_str() {
            "--email" => {
                email = rest.get(i + 1).cloned();
                i += 2;
            }
            "--name" => {
                name = rest.get(i + 1).cloned();
                i += 2;
            }
            "--password" => {
                password = rest.get(i + 1).cloned();
                i += 2;
            }
            "--admin" => {
                admin = true;
                i += 1;
            }
            _ => {
                config_args.push(rest[i].clone());
                i += 1;
            }
        }
    }
    let email = email.ok_or("--email is required")?;
    let name = name.ok_or("--name is required")?;
    let password = match password {
        Some(p) => p,
        None => {
            let generated = uuid_password();
            println!("generated password: {generated}");
            generated
        }
    };
    let overrides = cli_overrides(&config_args)?;
    let config = ServerConfig::resolve(overrides).map_err(|e| e.to_string())?;
    let auth_path = std::env::var_os("SKWAD_AUTH_FILE")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| config.library_root.join("auth").join("credentials.json"));
    let role = if admin {
        skwad_app_core::api::UserRole::Admin
    } else {
        skwad_app_core::api::UserRole::Member
    };
    let id = skwad_app_core::api::catalogue::upsert_user(&auth_path, &email, &name, &password, role)
        .map_err(|e| e.message)?;
    println!(
        "{} {email} ({name}) in {}",
        if admin { "administrator" } else { "member" },
        auth_path.display()
    );
    println!("account id: {id}");
    Ok(())
}

/// Picks `--key value` pairs out of `rest`, leaving the rest alone.
fn take_flags(rest: &[String], keys: &[&str]) -> (std::collections::HashMap<String, String>, Vec<String>) {
    let mut found = std::collections::HashMap::new();
    let mut remaining = Vec::new();
    let mut i = 0;
    while i < rest.len() {
        if keys.contains(&rest[i].as_str()) {
            if let Some(value) = rest.get(i + 1) {
                found.insert(rest[i].clone(), value.clone());
            }
            i += 2;
        } else {
            remaining.push(rest[i].clone());
            i += 1;
        }
    }
    (found, remaining)
}

/// Where a headless worker keeps its own state: models, downloads, machine id.
fn worker_home(flags: &std::collections::HashMap<String, String>) -> std::path::PathBuf {
    flags
        .get("--home")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| config::default_library_root().join("worker"))
}

/// `enrol --server URL --email E --password P --name N [--home DIR]` signs in
/// as an administrator and enrols this box, printing the token to keep.
fn enrol(rest: &[String]) -> Result<(), String> {
    let (flags, _) = take_flags(rest, &["--server", "--email", "--password", "--name", "--home"]);
    let server = flags.get("--server").ok_or("--server is required")?;
    let email = flags.get("--email").ok_or("--email is required")?;
    let name = flags.get("--name").ok_or("--name is required")?;
    let password = match flags.get("--password") {
        Some(p) => p.clone(),
        None => rpassword_prompt("password: ")?,
    };
    let home = worker_home(&flags);
    std::fs::create_dir_all(&home).map_err(|e| e.to_string())?;
    let machine_id = skwad_app_core::machine::load_or_create(&home);
    let enrolled = skwad_app_core::remote::enrol_with_credentials(server, email, &password, name, &machine_id)?;
    println!("enrolled {} as \"{}\"", enrolled.machine.id, enrolled.machine.name);
    println!("machine token (shown once):
{}", enrolled.token);
    println!("
run:  skwad-server worker --server {server} --token <token> --home {}", home.display());
    Ok(())
}

fn rpassword_prompt(prompt: &str) -> Result<String, String> {
    use std::io::Write;
    print!("{prompt}");
    std::io::stdout().flush().ok();
    let mut line = String::new();
    std::io::stdin().read_line(&mut line).map_err(|e| e.to_string())?;
    Ok(line.trim().to_string())
}

/// `worker --server URL --token T [--home DIR] [--ai-workers N]` runs analysis
/// slots for a server with no library of its own — a GPU box in a rack.
fn worker(rest: &[String]) -> Result<(), String> {
    use skwad_app_core::{MachineSettings, RemoteConfig, RemoteJobSource, WorkerPool};
    let (flags, _) = take_flags(rest, &["--server", "--token", "--home", "--ai-workers"]);
    let server = flags.get("--server").ok_or("--server is required")?.clone();
    let token = flags
        .get("--token")
        .cloned()
        .or_else(|| std::env::var("SKWAD_MACHINE_TOKEN").ok())
        .ok_or("--token (or SKWAD_MACHINE_TOKEN) is required")?;
    let home = worker_home(&flags);
    let paths = skwad_app_core::AppPaths::create(&home).map_err(|e| e.to_string())?;
    let machine_id = skwad_app_core::machine::load_or_create(&home);
    let machine_file = skwad_app_core::settings::machine_settings_path(&home);
    let mut machine = MachineSettings::load(&machine_file).unwrap_or_default();
    if let Some(n) = flags.get("--ai-workers") {
        machine.ai_workers = n.parse().map_err(|e| format!("--ai-workers `{n}`: {e}"))?;
        machine.save(&machine_file).map_err(|e| e.to_string())?;
    }
    tracing::info!(server = %server, machine = %machine_id, home = %home.display(), slots = machine.ai_workers, "starting a headless worker");

    let source = RemoteJobSource::connect(
        RemoteConfig {
            base_url: server,
            machine_token: token,
            machine_id,
        },
        paths.clone(),
        machine,
    )?;
    source.sync_models()?;
    let keeper = source.start_keeper();
    let sink: Arc<dyn skwad_app_core::ProgressSink> = Arc::new(LogSink);
    let pool = WorkerPool::start_remote(sink, source.clone(), paths);
    tracing::info!("worker running; Ctrl-C to stop");

    let stop = source.shutdown_flag();
    ctrlc_wait(stop);
    source.shutdown();
    pool.join();
    let _ = keeper.join();
    let status = source.status();
    tracing::info!(completed = status.jobs_completed, failed = status.jobs_failed, "worker stopped");
    Ok(())
}

/// Blocks until Ctrl-C, on a small runtime of its own.
fn ctrlc_wait(stop: Arc<std::sync::atomic::AtomicBool>) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime for the signal handler");
    runtime.block_on(async {
        let _ = tokio::signal::ctrl_c().await;
    });
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// A worker with no window: events become log lines.
struct LogSink;

impl skwad_app_core::ProgressSink for LogSink {
    fn emit(&self, event: &str, payload: serde_json::Value) {
        match event {
            skwad_app_core::events::JOB_FAILED => tracing::warn!(%payload, "job failed"),
            skwad_app_core::events::NOTICE => tracing::info!(%payload, "notice"),
            _ => tracing::debug!(event, %payload, "event"),
        }
    }
}

fn uuid_password() -> String {
    // 20 characters from a v4 UUID: enough entropy for a first sign-in, and
    // meant to be changed.
    uuid::Uuid::new_v4().simple().to_string()[..20].to_string()
}

fn init_console_logging() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_env("SKWAD_LOG").unwrap_or_else(|_| "info".into()))
        .try_init();
}

fn print_usage() {
    println!(
        "skwad-server — the SKWAD Media Organiser server\n\n\
         USAGE\n  \
           skwad-server [run] [flags]      run in the foreground (Ctrl-C to stop)\n  \
           skwad-server doctor [flags]     check database, library, models and workers\n  \
           skwad-server create-user --email E --name N [--password P] [--admin]\n  \
           skwad-server enrol --server URL --email E --password P --name N [--home DIR]\n  \
           skwad-server worker --server URL --token T [--home DIR] [--ai-workers N]\n"
    );
    #[cfg(windows)]
    println!(
        "  skwad-server install [flags]    register the Windows service with these flags (elevated)\n  \
           skwad-server uninstall | start | stop | status\n"
    );
    println!(
        "FLAGS (each has an SKWAD_SERVER_* environment variable and a line in {})\n  \
           --bind HOST:PORT            default 127.0.0.1:8420\n  \
           --library DIR               the library folder (models, auth, caches)\n  \
           --cache DIR                 keep rebuildable caches elsewhere\n  \
           --database-url URL          postgres://user:pass@host:port/db\n  \
           --media-roots A;B           folders shoots may live under (the folder browser jail)\n  \
           --tls-cert PEM --tls-key PEM\n  \
           --allowed-origins A,B       extra browser origins allowed to call the API\n  \
           --web-dir DIR               serve a built web bundle at /\n  \
           --ai-workers N              AI slots this box runs itself\n  \
           --local-analysis BOOL       false: analyse only on enrolled worker machines\n",
        config::default_config_path().display()
    );
    let _ = Arc::new(());
}
