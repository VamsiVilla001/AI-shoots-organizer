//! Runs `teo-server` as a child process on loopback, and keeps it running.
//!
//! The desktop app is a client of the same server the NAS edition runs. That is
//! the whole point of the refactor: one implementation of every command, not
//! two that drift. What the shell adds is a private instance nobody else can
//! reach:
//!
//! * bound to `127.0.0.1` on **port 0**, so the OS picks a free port and no two
//!   launches fight over one;
//! * a token generated per launch and passed on the command line, never written
//!   to disk — the file-based token is for the NAS case, where a person has to
//!   read it;
//! * killed when the window closes, restarted if it dies on its own.
//!
//! The webview still loads the *embedded* bundle rather than the server's copy,
//! so a first paint never waits on a port, and a broken static-file path cannot
//! stop the app from starting.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use rand::RngCore;

/// How long to wait for the child to publish its port before calling it dead.
const START_TIMEOUT: Duration = Duration::from_secs(30);

/// Restarts are for crashes, not for a server that cannot run at all; after
/// this many in quick succession the app says so instead of looping.
const MAX_RESTARTS: usize = 5;

/// Where the child is, and how to talk to it.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Endpoint {
    /// `http://127.0.0.1:<port>`, ready to be used as a base URL.
    pub base_url: String,
    pub token: String,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerStatus {
    /// `None` while starting, or after it has failed for good.
    pub endpoint: Option<Endpoint>,
    pub running: bool,
    /// Set when the server could not be started; shown to the user verbatim.
    pub error: Option<String>,
    pub restarts: usize,
}

struct Inner {
    endpoint: Option<Endpoint>,
    child: Option<Child>,
    error: Option<String>,
    restarts: usize,
}

pub struct Supervisor {
    inner: Mutex<Inner>,
    token: String,
    data_dir: PathBuf,
    executable: PathBuf,
    /// Bundle resources, where a packaged build keeps the ONNX models.
    resources: Option<PathBuf>,
    stopping: Arc<AtomicBool>,
}

impl Supervisor {
    /// Starts the child and blocks until it is listening, or fails.
    pub fn start(data_dir: PathBuf, resources: Option<PathBuf>) -> Self {
        let token = generate_token();
        let executable = locate_server();

        let supervisor = Self {
            inner: Mutex::new(Inner { endpoint: None, child: None, error: None, restarts: 0 }),
            token,
            data_dir,
            executable: executable.clone().unwrap_or_default(),
            resources,
            stopping: Arc::new(AtomicBool::new(false)),
        };

        match executable {
            Some(path) => {
                tracing::info!(server = %path.display(), "starting the local server");
                supervisor.spawn_once();
            }
            None => {
                let message = "the teo-server executable was not found next to the application";
                tracing::error!("{message}");
                supervisor.inner.lock().error = Some(message.to_string());
            }
        }

        supervisor
    }

    pub fn status(&self) -> ServerStatus {
        let inner = self.inner.lock();
        ServerStatus {
            endpoint: inner.endpoint.clone(),
            running: inner.child.is_some() && inner.endpoint.is_some(),
            error: inner.error.clone(),
            restarts: inner.restarts,
        }
    }

    /// Spawns the child and waits for it to publish its address.
    fn spawn_once(&self) -> bool {
        // The child writes the address it actually bound here, because a parent
        // that asked for port 0 has no other way to learn it, and pre-picking a
        // port would race with everything else on the machine.
        let port_file = self.data_dir.join("server.addr");
        let _ = std::fs::remove_file(&port_file);

        let mut command = Command::new(&self.executable);
        command
            .arg("--bind")
            .arg("127.0.0.1:0")
            .arg("--data-dir")
            .arg(&self.data_dir)
            .arg("--token")
            .arg(&self.token)
            .arg("--port-file")
            .arg(&port_file)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        if let Some(resources) = &self.resources {
            let models = resources.join("models");
            if models.is_dir() {
                command.arg("--seed-models-from").arg(models);
            }
        }

        // No media roots: a desktop user picks folders with the native dialog,
        // which is already an explicit choice, and confining them would break
        // opening a shoot from any drive.
        #[cfg(windows)]
        {
            // Without this the child briefly flashes a console window.
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NO_WINDOW);
        }

        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(e) => {
                let message = format!("could not start the local server: {e}");
                tracing::error!("{message}");
                self.inner.lock().error = Some(message);
                return false;
            }
        };

        // The child's logs are the app's logs; without this they vanish.
        forward_output(&mut child);

        match wait_for_address(&port_file, &mut child) {
            Ok(address) => {
                let mut inner = self.inner.lock();
                inner.endpoint = Some(Endpoint {
                    base_url: format!("http://{address}"),
                    token: self.token.clone(),
                });
                inner.child = Some(child);
                inner.error = None;
                tracing::info!(address = %address, "local server is listening");
                true
            }
            Err(message) => {
                let _ = child.kill();
                tracing::error!("{message}");
                let mut inner = self.inner.lock();
                inner.error = Some(message);
                inner.endpoint = None;
                false
            }
        }
    }

    /// Watches the child and restarts it if it exits on its own.
    ///
    /// Spawned as a thread by the caller so the setup hook does not block.
    pub fn supervise(self: &Arc<Self>) {
        let supervisor = Arc::clone(self);
        let _ = std::thread::Builder::new().name("teo-supervisor".into()).spawn(move || {
            loop {
                std::thread::sleep(Duration::from_millis(500));
                if supervisor.stopping.load(Ordering::Relaxed) {
                    return;
                }

                let exited = {
                    let mut inner = supervisor.inner.lock();
                    match inner.child.as_mut() {
                        Some(child) => match child.try_wait() {
                            Ok(Some(status)) => Some(status),
                            Ok(None) => None,
                            // A child we cannot query is a child we no longer
                            // control; treat it as gone and start a fresh one.
                            Err(_) => Some(Default::default()),
                        },
                        None => None,
                    }
                };

                let Some(status) = exited else { continue };
                if supervisor.stopping.load(Ordering::Relaxed) {
                    return;
                }

                let restarts = {
                    let mut inner = supervisor.inner.lock();
                    inner.child = None;
                    inner.endpoint = None;
                    inner.restarts += 1;
                    inner.restarts
                };

                tracing::warn!(?status, restarts, "the local server exited; restarting");

                if restarts > MAX_RESTARTS {
                    let message = format!(
                        "the local server stopped {restarts} times; \
                         check the log at logs/teo.log for why"
                    );
                    supervisor.inner.lock().error = Some(message);
                    return;
                }

                // A little backoff, so a server that dies instantly does not
                // spin the CPU while it fails.
                std::thread::sleep(Duration::from_millis(250 * restarts as u64));
                supervisor.spawn_once();
            }
        });
    }

    /// Stops the child. Called when the window closes.
    pub fn stop(&self) {
        self.stopping.store(true, Ordering::Relaxed);
        let mut inner = self.inner.lock();
        if let Some(mut child) = inner.child.take() {
            tracing::info!("stopping the local server");
            let _ = child.kill();
            let _ = child.wait();
        }
        inner.endpoint = None;
    }
}

fn generate_token() -> String {
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Polls for the address file, giving up if the child dies or stalls.
fn wait_for_address(port_file: &Path, child: &mut Child) -> Result<String, String> {
    let deadline = Instant::now() + START_TIMEOUT;

    while Instant::now() < deadline {
        if let Ok(Some(status)) = child.try_wait() {
            return Err(format!("the local server exited before it started listening ({status})"));
        }
        if let Ok(text) = std::fs::read_to_string(port_file) {
            let address = text.trim().to_string();
            if !address.is_empty() {
                let _ = std::fs::remove_file(port_file);
                return Ok(address);
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    Err("the local server did not start listening within 30 seconds".to_string())
}

/// Pipes the child's stdout and stderr into this process's log.
fn forward_output(child: &mut Child) {
    for (name, pipe) in [
        ("stdout", child.stdout.take().map(|p| Box::new(p) as Box<dyn std::io::Read + Send>)),
        ("stderr", child.stderr.take().map(|p| Box::new(p) as Box<dyn std::io::Read + Send>)),
    ] {
        if let Some(pipe) = pipe {
            let _ = std::thread::Builder::new().name(format!("teo-server-{name}")).spawn(move || {
                for line in BufReader::new(pipe).lines().map_while(Result::ok) {
                    tracing::info!(target: "teo_server", "{line}");
                }
            });
        }
    }
}

/// Finds the server executable.
///
/// Beside the application first, which is where both the installer and a
/// `cargo build` put it; then the bundle's resource directory, which is where a
/// macOS `.app` keeps extra binaries.
fn locate_server() -> Option<PathBuf> {
    let name = if cfg!(windows) { "teo-server.exe" } else { "teo-server" };

    let mut candidates = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join(name));
            // macOS: Contents/MacOS/teo-desktop → Contents/Resources/teo-server
            candidates.push(dir.join("../Resources").join(name));
        }
    }

    candidates.into_iter().find(|path| path.is_file()).map(|path| {
        // Canonicalise so the child's own `current_exe` lookups behave.
        path.canonicalize().unwrap_or(path)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_token_is_long_and_different_every_time() {
        let a = generate_token();
        let b = generate_token();
        assert_eq!(a.len(), 64, "32 random bytes, hex encoded");
        assert_ne!(a, b, "a per-launch token must not repeat");
    }

    #[test]
    fn a_missing_server_binary_is_reported_not_panicked() {
        let dir = std::env::temp_dir().join(format!("teo-sup-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        let supervisor = Supervisor {
            inner: Mutex::new(Inner { endpoint: None, child: None, error: None, restarts: 0 }),
            token: "t".into(),
            data_dir: dir.clone(),
            executable: dir.join("does-not-exist"),
            resources: None,
            stopping: Arc::new(AtomicBool::new(false)),
        };

        assert!(!supervisor.spawn_once());
        let status = supervisor.status();
        assert!(!status.running);
        assert!(status.error.is_some(), "the UI needs something to show");
        assert!(status.endpoint.is_none());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn waiting_gives_up_on_a_child_that_dies() {
        // `cmd /c exit 3` stands in for a server that fails immediately.
        let mut child = if cfg!(windows) {
            Command::new("cmd").args(["/C", "exit", "3"]).spawn().unwrap()
        } else {
            Command::new("sh").args(["-c", "exit 3"]).spawn().unwrap()
        };

        let missing = std::env::temp_dir().join("teo-never-written.addr");
        let _ = std::fs::remove_file(&missing);

        let result = wait_for_address(&missing, &mut child);
        assert!(result.is_err());
        assert!(
            result.unwrap_err().contains("exited before it started listening"),
            "the message has to say the child died, not that it timed out"
        );
    }
}
