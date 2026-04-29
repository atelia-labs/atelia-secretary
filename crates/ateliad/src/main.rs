use anyhow::Result;
use atelia_core::PolicyState;
use tracing::info;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            std::env::var("RUST_LOG").unwrap_or_else(|_| "ateliad=info".to_string()),
        )
        .init();

    let auto_merge = PolicyState::Blocked;
    info!(?auto_merge, "Atelia Secretary daemon starting");
    info!("RPC server is not wired yet; daemon skeleton is alive");

    tokio::signal::ctrl_c().await?;
    info!("Atelia Secretary daemon stopping");
    Ok(())
}
