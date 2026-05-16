mod backend;
mod config;
mod embed;
mod networks;
mod status;
mod structs;
mod traits;
mod web_server;

use anyhow::{Context, Result};
use backend::WpaCtrlBackend;
use config::{AppConfig, CliOptions};
use networks::{KnownNetworks, NetworkStore};
use status::{ErrorReason, StatusPublisher};
use std::sync::Arc;
use tokio::sync::Mutex;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    tracing::info!("starting wlan0-bootstrap");

    let options = CliOptions::parse()?;
    let config = Arc::new(AppConfig::load(&options)?);
    let status = StatusPublisher::new(config.status_path(), config.event_socket_path()).await?;
    status.start_event_server().await?;

    let store = NetworkStore::new(config.networks_path());
    let known_networks = Arc::new(Mutex::new(
        store
            .load()
            .await
            .context("failed to load known networks")?,
    ));

    let backend = Arc::new(WpaCtrlBackend::new(config.clone(), status.clone()));
    backend.prepare().await?;

    loop {
        let scanned = backend.scan().await.unwrap_or_else(|err| {
            tracing::warn!("scan failed: {}", err);
            Vec::new()
        });

        if try_known_networks(&backend, &known_networks, &scanned, &store).await? {
            continue;
        }

        status
            .set_error(
                ErrorReason::NoKnownNetwork,
                "no known Wi-Fi network is currently reachable",
                None,
            )
            .await?;

        backend.start_provisioning_ap().await?;
        web_server::run_server(
            backend.clone(),
            status.clone(),
            known_networks.clone(),
            store.clone(),
            scanned,
        )
        .await?;

        if let Some(ssid) = status.snapshot().await.ssid {
            backend.monitor_until_disconnected(&ssid).await;
        }
    }
}

async fn try_known_networks(
    backend: &Arc<WpaCtrlBackend>,
    known_networks: &Arc<Mutex<KnownNetworks>>,
    scanned: &[structs::Network],
    store: &NetworkStore,
) -> Result<bool> {
    let candidates = {
        let guard = known_networks.lock().await;
        guard
            .candidates_for_scan(scanned)
            .into_iter()
            .cloned()
            .collect::<Vec<_>>()
    };

    for known in candidates {
        match backend.connect_known(&known).await {
            Ok(info) => {
                let mut guard = known_networks.lock().await;
                guard.upsert_success(&structs::ConnectionRequest {
                    ssid: known.ssid.clone(),
                    password: known.psk.clone(),
                });
                store.save(&guard).await?;
                drop(guard);
                backend.monitor_until_disconnected(&info.ssid).await;
                return Ok(true);
            }
            Err(err) => {
                tracing::warn!("failed to connect known network {}: {}", known.ssid, err);
            }
        }
    }

    Ok(false)
}
