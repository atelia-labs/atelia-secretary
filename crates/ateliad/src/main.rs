mod service;

use anyhow::Result;
use tracing::info;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(std::env::var("RUST_LOG").unwrap_or_else(|_| "ateliad=info".to_string()))
        .init();

    let mut service = service::SecretaryService::new();
    info!("Atelia Secretary daemon starting");

    let health = service.health();
    info!(
        version = %health.daemon_version,
        protocol = %health.protocol_version,
        storage = %health.storage_version,
        status = ?health.daemon_status,
        "Daemon service initialized; RPC listener not wired yet"
    );

    tokio::signal::ctrl_c().await?;
    info!("Atelia Secretary daemon stopping");

    service.set_stopping();
    Ok(())
}
