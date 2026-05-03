mod rpc;
mod service;
mod transport;

use anyhow::Context;
use anyhow::Result;
use std::future::Future;
use std::sync::Arc;
use tokio::sync::{oneshot, RwLock};
use tracing::info;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(std::env::var("RUST_LOG").unwrap_or_else(|_| "ateliad=info".to_string()))
        .init();

    let mut service = service::SecretaryService::new();
    info!("Atelia Secretary daemon starting");

    service.set_running();
    let rpc_server = Arc::new(RwLock::new(rpc::SecretaryRpcServer::new(service)));
    let health = rpc_server.read().await.health(rpc::HealthRequest);

    let (listen_addr, explicit_addr) = transport::listen_addr()?;
    if !explicit_addr && !transport::is_loopback(&listen_addr) {
        return Err(anyhow::anyhow!(
            "default listener address resolved to non-loopback {listen_addr}"
        ));
    }
    if explicit_addr && !transport::is_loopback(&listen_addr) {
        tracing::warn!(
            listen_addr = %listen_addr,
            "daemon is bound to explicit non-loopback address via ATELIA_DAEMON_LISTEN_ADDR"
        );
    }

    let listener = transport::bind_listener(listen_addr).await?;
    let bound_addr = listener
        .local_addr()
        .with_context(|| format!("failed to read bound listener address for {listen_addr}"))?;

    info!(
        version = %health.daemon_version,
        protocol = %health.protocol_version,
        storage = %health.storage_version,
        status = %health.daemon_status,
        listening = %bound_addr,
        "Daemon RPC transport listener ready"
    );

    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let listener_task = tokio::spawn(transport::run_listener(
        rpc_server.clone(),
        listener,
        shutdown_rx,
    ));

    run_until_shutdown(
        rpc_server,
        listener_task,
        shutdown_tx,
        tokio::signal::ctrl_c(),
    )
    .await?;

    Ok(())
}

async fn run_until_shutdown<S>(
    rpc_server: Arc<RwLock<rpc::SecretaryRpcServer>>,
    listener_task: tokio::task::JoinHandle<Result<()>>,
    shutdown_tx: oneshot::Sender<()>,
    shutdown_signal: S,
) -> Result<()>
where
    S: Future<Output = std::io::Result<()>> + Send,
{
    tokio::pin!(shutdown_signal);
    tokio::pin!(listener_task);

    tokio::select! {
        signal_result = &mut shutdown_signal => {
            info!("Atelia Secretary daemon stopping");
            rpc_server.write().await.service_mut().set_stopping();
            let _ = shutdown_tx.send(());
            signal_result.context("failed to await shutdown signal")?;

            let listener_result = listener_task
                .await
                .context("listener task panicked during shutdown")?;
            if let Err(error) = &listener_result {
                tracing::error!(error = %error, "listener task encountered an error while shutting down");
            }
            listener_result.with_context(|| "listener task encountered an error during shutdown")?;
            Ok(())
        }
        listener_result = &mut listener_task => {
            rpc_server.write().await.service_mut().set_stopping();
            let _ = shutdown_tx.send(());

            match listener_result {
                Ok(Ok(())) => {
                    tracing::error!("listener task completed before shutdown");
                    Err(anyhow::anyhow!("listener task completed before shutdown"))
                        .context("listener task completed before shutdown")
                }
                Ok(Err(error)) => {
                    tracing::error!(error = %error, "listener task encountered an error before shutdown");
                    Err(error).context("listener task encountered an error before shutdown")
                }
                Err(error) => {
                    tracing::error!(error = %error, "listener task panicked before shutdown");
                    Err(error).context("listener task panicked before shutdown")
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::time::{sleep, timeout};

    #[tokio::test]
    async fn run_until_shutdown_uses_ctrl_signal_to_stop_listener() {
        let service = service::SecretaryService::new();
        let rpc_server = Arc::new(RwLock::new(rpc::SecretaryRpcServer::new(service)));

        let (shutdown_tx, _shutdown_rx) = oneshot::channel();
        let listener_task = tokio::spawn(async move {
            _shutdown_rx.await.expect("shutdown signal");
            Ok(())
        });

        let (ctrl_tx, ctrl_rx) = oneshot::channel::<()>();
        let handle = tokio::spawn({
            let rpc_server = rpc_server.clone();
            async move {
                run_until_shutdown(rpc_server, listener_task, shutdown_tx, async move {
                    ctrl_rx.await.expect("ctrl signal");
                    Ok(())
                })
                .await
            }
        });

        sleep(Duration::from_millis(25)).await;
        ctrl_tx.send(()).expect("send ctrl signal");

        timeout(Duration::from_secs(1), handle)
            .await
            .expect("run_until_shutdown did not complete")
            .expect("supervisor task completed")
            .expect("shutdown should be clean");

        assert_eq!(
            rpc_server
                .read()
                .await
                .health(rpc::HealthRequest)
                .daemon_status,
            "stopping"
        );
    }

    #[tokio::test]
    async fn run_until_shutdown_returns_error_if_listener_fails_before_signal() {
        let service = service::SecretaryService::new();
        let rpc_server = Arc::new(RwLock::new(rpc::SecretaryRpcServer::new(service)));
        let (shutdown_tx, _shutdown_rx) = oneshot::channel();

        let listener_task = tokio::spawn(async { Err(anyhow::anyhow!("listener failed")) });

        let result = run_until_shutdown(
            rpc_server.clone(),
            listener_task,
            shutdown_tx,
            std::future::pending::<std::io::Result<()>>(),
        )
        .await;

        let err = result.expect_err("listener failure should propagate");
        assert_eq!(
            rpc_server
                .read()
                .await
                .health(rpc::HealthRequest)
                .daemon_status,
            "stopping"
        );
        assert!(err.to_string().contains("before shutdown"));
    }

    #[tokio::test]
    async fn run_until_shutdown_returns_error_if_listener_panics_before_signal() {
        let service = service::SecretaryService::new();
        let rpc_server = Arc::new(RwLock::new(rpc::SecretaryRpcServer::new(service)));
        let (shutdown_tx, _shutdown_rx) = oneshot::channel::<()>();

        let listener_task = tokio::spawn(async {
            panic!("listener panicked");
        });

        let result = run_until_shutdown(
            rpc_server.clone(),
            listener_task,
            shutdown_tx,
            std::future::pending::<std::io::Result<()>>(),
        )
        .await;

        let err = result.expect_err("listener panic should propagate");
        assert_eq!(
            rpc_server
                .read()
                .await
                .health(rpc::HealthRequest)
                .daemon_status,
            "stopping"
        );
        assert!(err.to_string().contains("before shutdown"));
    }

    #[tokio::test]
    async fn run_until_shutdown_returns_error_if_listener_stops_before_signal() {
        let service = service::SecretaryService::new();
        let rpc_server = Arc::new(RwLock::new(rpc::SecretaryRpcServer::new(service)));
        let (shutdown_tx, _shutdown_rx) = oneshot::channel::<()>();

        let listener_task = tokio::spawn(async { Ok(()) });

        let result = run_until_shutdown(
            rpc_server.clone(),
            listener_task,
            shutdown_tx,
            std::future::pending::<std::io::Result<()>>(),
        )
        .await;

        let err = result.expect_err("listener completion should propagate as an error");
        assert_eq!(
            rpc_server
                .read()
                .await
                .health(rpc::HealthRequest)
                .daemon_status,
            "stopping"
        );
        assert!(err
            .to_string()
            .contains("listener task completed before shutdown"));
    }
}
