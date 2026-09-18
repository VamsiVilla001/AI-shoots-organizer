//! `skwad-server` — run, diagnose, install.
//!
//! ```text
//! skwad-server [run] [--bind …] [--library …] [--media-roots …] [--tls-cert … --tls-key …]
//! skwad-server doctor            check the database, library, models and workers, then exit
//! skwad-server create-user --email … --name … [--password …] [--admin]
//! skwad-server install …         register the Windows service with these settings (elevated)
//! skwad-server uninstall | start | stop | status
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
        Some(c @ ("run" | "doctor" | "create-user" | "install" | "uninstall" | "start" | "stop" | "status" | "help" | "--help" | "-h")) => (c, &args[1..]),
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
           skwad-server create-user --email E --name N [--password P] [--admin]\n"
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
           --ai-workers N              AI slots this box runs itself\n",
        config::default_config_path().display()
    );
    let _ = Arc::new(());
}
