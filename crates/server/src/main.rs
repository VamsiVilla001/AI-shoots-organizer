//! `teo-server` — the headless front door.

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("TEO_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,teo=debug")),
        )
        .init();

    let config = teo_server::ServerConfig::from_env();
    tracing::info!(
        bind = %config.bind,
        data = %config.data_dir.display(),
        media_roots = config.media_roots.len(),
        "teo-server scaffold; HTTP routes are not implemented yet"
    );
}
