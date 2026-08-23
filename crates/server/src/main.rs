//! `teo-server` — the headless front door.
//!
//! Configuration comes from flags first, environment second, so a container can
//! set `TEO_*` and a developer can override one value on the command line:
//!
//! ```text
//! teo-server --bind 127.0.0.1:8420 --data-dir ./.teo --media-roots D:\shoots
//! ```

use std::path::PathBuf;

use teo_server::config::parse_path_list;
use teo_server::ServerConfig;

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("TEO_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,teo=debug")),
        )
        .init();

    let config = match parse_args(ServerConfig::from_env())? {
        Some(config) => config,
        // `--help` printed usage; nothing to run.
        None => return Ok(()),
    };

    let (state, workers) = teo_server::boot(config)?;

    // The core is synchronous and owns its own threads; the runtime here exists
    // only for the HTTP layer.
    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    let result = runtime.block_on(teo_server::serve(std::sync::Arc::clone(&state)));

    // Let workers finish the job they are on; anything queued resumes next
    // start, which is what the persistent queue is for.
    state.core.begin_shutdown();
    workers.join();

    result
}

const USAGE: &str = "\
teo-server — Esports AI Media Organiser, served over HTTP

Options:
  --bind <addr:port>        Address to listen on         [env TEO_BIND]
  --data-dir <path>         Database, thumbnails, models [env TEO_DATA_DIR]
  --media-roots <list>      Folders shoots may come from [env TEO_MEDIA_ROOTS]
  --output-roots <list>     Folders exports may write to [env TEO_OUTPUT_ROOTS]
  --token <token>           Shared access token          [env TEO_TOKEN]
  --web-dir <path>          Built React bundle to serve  [env TEO_WEB_DIR]
  -h, --help                Print this message

Lists are comma-separated, or semicolon-separated on Windows.
Without a token one is generated into <data-dir>/token on first run.
";

/// Returns `None` when the program should exit after printing usage.
fn parse_args(mut config: ServerConfig) -> anyhow::Result<Option<ServerConfig>> {
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        let mut value = || -> anyhow::Result<String> {
            args.next().ok_or_else(|| anyhow::anyhow!("{flag} needs a value"))
        };

        match flag.as_str() {
            "--bind" => config.bind = value()?,
            "--data-dir" => config.data_dir = PathBuf::from(value()?),
            "--media-roots" => config.media_roots = parse_path_list(&value()?),
            "--output-roots" => config.output_roots = parse_path_list(&value()?),
            "--token" => config.token = Some(value()?),
            "--web-dir" => config.web_dir = Some(PathBuf::from(value()?)),
            "-h" | "--help" => {
                print!("{USAGE}");
                return Ok(None);
            }
            other => anyhow::bail!("unknown option {other}\n\n{USAGE}"),
        }
    }
    Ok(Some(config))
}
