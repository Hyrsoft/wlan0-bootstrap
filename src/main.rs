mod backend;
mod config;
mod device_profile;
mod discovery;
mod embed;
mod networks;
mod status;
mod structs;
mod traits;
mod web_server;

use anyhow::{Context, Result};
use backend::{ConnectedInfo, WpaCtrlBackend};
use config::{AppConfig, CliOptions};
use networks::{KnownNetworks, NetworkStore};
use status::{ErrorReason, StatusPublisher};
use std::sync::Arc;
use tokio::sync::Mutex;
use web_server::ProvisioningExit;

#[tokio::main]
async fn main() -> Result<()> {
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(env_filter).init();

    tracing::info!("starting wlan0-bootstrap");

    let options = CliOptions::parse()?;
    let config = Arc::new(AppConfig::load(&options)?);
    let status = StatusPublisher::new(config.status_path(), config.event_socket_path()).await?;
    status.start_event_server().await?;
    initialize_discovery_status(&config, &status).await;

    let store = NetworkStore::new(config.networks_path());
    // 已知网络是本程序自己的持久化数据。
    // 当前阶段按用户要求保存可直接传给 wpa_supplicant 的密码字符串，
    // 不额外调用派生工具，也不在程序内实现 PSK 派生。
    let known_networks = Arc::new(Mutex::new(
        store
            .load()
            .await
            .context("failed to load known networks")?,
    ));

    let backend = Arc::new(WpaCtrlBackend::new(config.clone(), status.clone()));
    if let Err(err) = backend.prepare().await {
        backend.shutdown().await;
        return Err(err);
    }

    let result = run_loop(backend.clone(), status, known_networks, store).await;
    if result.is_err() {
        backend.shutdown().await;
    }
    result
}

async fn initialize_discovery_status(config: &Arc<AppConfig>, status: &Arc<StatusPublisher>) {
    if !config.discovery.mdns_enabled || !config.discovery.http_service_enabled {
        let _ = status.set_mdns_disabled().await;
        return;
    }

    match discovery::resolve_discovery_hostname(config).await {
        Ok(hostname) => {
            let _ = status.set_discovery_hostname(hostname).await;
        }
        Err(err) => {
            tracing::warn!("failed to resolve discovery hostname: {}", err);
            let _ = status.set_mdns_failed(None, err.to_string()).await;
        }
    }
}

async fn run_loop(
    backend: Arc<WpaCtrlBackend>,
    status: Arc<StatusPublisher>,
    known_networks: Arc<Mutex<KnownNetworks>>,
    store: NetworkStore,
) -> Result<()> {
    loop {
        // 主循环固定为单射频 TDM：
        // 先 STA 扫描，再连接已知网络；都失败后才进入 Soft AP 配网。
        let scanned = backend.scan().await.unwrap_or_else(|err| {
            tracing::warn!("scan failed: {}", err);
            Vec::new()
        });

        if let Some(info) = try_known_networks(&backend, &known_networks, &scanned, &store).await? {
            handle_connected(&backend, &status, info).await;
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
        let provisioning_exit = web_server::run_server(
            backend.clone(),
            status.clone(),
            known_networks.clone(),
            store.clone(),
        )
        .await?;

        match provisioning_exit {
            ProvisioningExit::Connected => {
                // Web 服务只在连接成功后退出；这里拿到的是目标 STA SSID。
                // 空闲超时会走 IdleTimeout 分支，避免误监控 AP SSID。
                let snapshot = status.snapshot().await;
                if let Some(ssid) = snapshot.ssid {
                    handle_connected(
                        &backend,
                        &status,
                        ConnectedInfo {
                            ssid,
                            ip: snapshot.address,
                        },
                    )
                    .await;
                }
            }
            ProvisioningExit::IdleTimeout => {
                tracing::info!("provisioning idle timeout reached; restarting scan cycle");
                let _ = backend.stop_provisioning_ap().await;
            }
        }
    }
}

async fn handle_connected(
    backend: &Arc<WpaCtrlBackend>,
    status: &Arc<StatusPublisher>,
    info: ConnectedInfo,
) {
    let port = match backend.config().bind_addr() {
        Ok(addr) => addr.port(),
        Err(err) => {
            let _ = status.set_mdns_failed(None, err.to_string()).await;
            backend.monitor_until_disconnected(&info.ssid).await;
            backend.stop_discovery().await;
            return;
        }
    };

    let connected_server = match web_server::run_connected_server(status.clone(), port).await {
        Ok(server) => Some(server),
        Err(err) => {
            tracing::warn!("failed to start connected web server: {}", err);
            let _ = status
                .set_mdns_failed(None, format!("http server unavailable: {}", err))
                .await;
            None
        }
    };

    if connected_server.is_some() {
        let _ = backend.publish_connected_discovery(&info).await;
    }
    backend.monitor_until_disconnected(&info.ssid).await;
    backend.stop_discovery().await;

    if let Some(server) = connected_server
        && let Err(err) = server.stop().await
    {
        tracing::warn!("failed to stop connected web server: {}", err);
    }
}

async fn try_known_networks(
    backend: &Arc<WpaCtrlBackend>,
    known_networks: &Arc<Mutex<KnownNetworks>>,
    scanned: &[structs::Network],
    store: &NetworkStore,
) -> Result<Option<ConnectedInfo>> {
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
                // 已知网络连接成功后只更新最近成功时间和 disabled 状态。
                // known.psk 字段名沿用蓝图，但当前保存的是明文密码或历史 raw PSK。
                let mut guard = known_networks.lock().await;
                guard.upsert_success(&structs::ConnectionRequest {
                    ssid: known.ssid.clone(),
                    password: known.psk.clone(),
                });
                store.save(&guard).await?;
                drop(guard);
                return Ok(Some(info));
            }
            Err(err) => {
                tracing::warn!("failed to connect known network {}: {}", known.ssid, err);
            }
        }
    }

    Ok(None)
}
