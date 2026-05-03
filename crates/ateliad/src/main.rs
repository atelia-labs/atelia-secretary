mod rpc;
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

    service.set_running();
    let mut rpc_server = rpc::SecretaryRpcServer::new(service);
    let health = rpc_server.health(rpc::HealthRequest);
    info!(
        version = %health.daemon_version,
        protocol = %health.protocol_version,
        storage = %health.storage_version,
        status = %health.daemon_status,
        transport_blocker = ?rpc_server.transport_blocker(),
        "Daemon RPC boundary initialized; external listener not wired yet"
    );

    tokio::signal::ctrl_c().await?;
    info!("Atelia Secretary daemon stopping");

    rpc_server.service_mut().set_stopping();
    Ok(())
}
