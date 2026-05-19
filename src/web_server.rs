use crate::backend::WpaCtrlBackend;
use crate::embed::EmbedFrontend;
use crate::networks::{KnownNetworks, NetworkStore};
use crate::status::StatusPublisher;
use crate::structs::{ConnectAccepted, ConnectionRequest};
use crate::traits::UiAssetProvider;
use anyhow::Context;
use axum::{
    Router,
    body::Body,
    extract::State,
    http::{StatusCode, Uri, header},
    response::{IntoResponse, Json, Response},
    routing::{get, post},
};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use tokio::net::TcpListener;
use tokio::sync::{Mutex, oneshot};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProvisioningExit {
    Connected,
    IdleTimeout,
}

struct AppState {
    backend: Arc<WpaCtrlBackend>,
    status: Arc<StatusPublisher>,
    known_networks: Arc<Mutex<KnownNetworks>>,
    store: NetworkStore,
    ui_provider: Arc<dyn UiAssetProvider>,
    connect_in_progress: AtomicBool,
    shutdown: Mutex<Option<oneshot::Sender<()>>>,
}

pub async fn run_server(
    backend: Arc<WpaCtrlBackend>,
    status: Arc<StatusPublisher>,
    known_networks: Arc<Mutex<KnownNetworks>>,
    store: NetworkStore,
) -> anyhow::Result<ProvisioningExit> {
    // Web 服务只在配网窗口内运行。
    // 连接成功或空闲超时都会触发 graceful shutdown，把控制权交还给主循环。
    let (connected_tx, connected_rx) = oneshot::channel();
    let (stop_tx, stop_rx) = oneshot::channel();
    let app_state = Arc::new(AppState {
        backend: backend.clone(),
        status,
        known_networks,
        store,
        ui_provider: Arc::new(EmbedFrontend::new()),
        connect_in_progress: AtomicBool::new(false),
        shutdown: Mutex::new(Some(connected_tx)),
    });

    let app = Router::new()
        .route("/api/scan", get(api_scan))
        .route("/api/status", get(api_status))
        .route("/api/connect", post(api_connect))
        .route("/api/backend_kind", get(api_backend_kind))
        .route("/generate_204", get(handle_captive_portal))
        .fallback(get(serve_static_asset))
        .with_state(app_state);

    let bind_addr = backend.config().bind_addr()?;
    tracing::info!("provisioning web server listening on {}", bind_addr);

    let listener = TcpListener::bind(bind_addr).await?;
    let mut server = tokio::spawn(async move {
        axum::serve(listener, app.into_make_service())
            .with_graceful_shutdown(async move {
                let _ = stop_rx.await;
            })
            .await
    });

    let idle_timeout = tokio::time::sleep(std::time::Duration::from_secs(
        backend.config().timeouts.provisioning_idle_seconds,
    ));

    let exit = tokio::select! {
        result = &mut server => {
            result.context("provisioning web server task failed")??;
            return Ok(ProvisioningExit::Connected);
        }
        _ = connected_rx => ProvisioningExit::Connected,
        _ = idle_timeout => ProvisioningExit::IdleTimeout,
    };

    let _ = stop_tx.send(());
    server
        .await
        .context("provisioning web server task failed")??;
    Ok(exit)
}

async fn api_scan(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    // 返回 backend 维护的扫描缓存。
    // 单射频设备此时正在提供 Soft AP，不在这里重新触发 STA 扫描。
    let networks = state.backend.cached_networks().await;
    tracing::info!(
        "web api scan requested; returning {} cached networks",
        networks.len()
    );
    (StatusCode::OK, Json(networks))
}

async fn api_status(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    (StatusCode::OK, Json(state.status.snapshot().await))
}

async fn api_backend_kind() -> impl IntoResponse {
    (
        StatusCode::OK,
        Json(serde_json::json!({ "kind": "single_radio_tdm" })),
    )
}

async fn api_connect(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<ConnectionRequest>,
) -> impl IntoResponse {
    // /api/connect 只表示请求已接收。
    // 真实连接结果通过 /api/status 轮询，避免 HTTP 请求长时间挂起。
    tracing::info!("web api connect requested: ssid={}", payload.ssid);
    if state
        .connect_in_progress
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        tracing::warn!(
            "web api connect rejected because another attempt is running: ssid={}",
            payload.ssid
        );
        return (
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "status": "busy",
                "message": "connection attempt already in progress"
            })),
        )
            .into_response();
    }

    let state_for_task = state.clone();
    tokio::spawn(async move {
        tracing::info!("provisioning connect task started: ssid={}", payload.ssid);
        let result = state_for_task
            .backend
            .connect_from_provisioning(&payload)
            .await;

        if result.is_ok() {
            tracing::info!("provisioning connect task succeeded: ssid={}", payload.ssid);
            {
                // 当前阶段不调用额外派生工具，也不在程序内实现 PSK 派生。
                // 连接成功后保存用户提交的密码字符串，后续自动连接继续交给 wpa_supplicant 处理。
                let mut guard = state_for_task.known_networks.lock().await;
                guard.upsert_success(&payload);
                if let Err(err) = state_for_task.store.save(&guard).await {
                    tracing::error!("failed to save known networks: {}", err);
                }
            }

            if let Some(sender) = state_for_task.shutdown.lock().await.take() {
                let _ = sender.send(());
            }
        } else if let Err(err) = &result {
            tracing::warn!(
                "provisioning connect task failed: ssid={} error={}",
                payload.ssid,
                err
            );
        }

        state_for_task
            .connect_in_progress
            .store(false, Ordering::SeqCst);
    });

    (
        StatusCode::OK,
        Json(ConnectAccepted {
            status: "accepted",
            message: "connection request accepted",
        }),
    )
        .into_response()
}

async fn handle_captive_portal() -> impl IntoResponse {
    (StatusCode::NO_CONTENT, "")
}

async fn serve_static_asset(State(state): State<Arc<AppState>>, uri: Uri) -> impl IntoResponse {
    let mut path = uri.path().trim_start_matches('/').to_string();
    if path.is_empty() {
        path = "index.html".to_string();
    }

    match state.ui_provider.get_asset(&path).await {
        Ok((data, mime)) => Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, mime)
            .body(Body::from(data))
            .unwrap_or_else(|_| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Failed to build response",
                )
                    .into_response()
            }),
        Err(err) => {
            tracing::warn!("asset not found {}: {}", path, err);
            (StatusCode::NOT_FOUND, "Not Found").into_response()
        }
    }
}
