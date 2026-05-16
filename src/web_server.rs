use crate::backend::WpaCtrlBackend;
use crate::embed::EmbedFrontend;
use crate::networks::{KnownNetworks, NetworkStore};
use crate::status::StatusPublisher;
use crate::structs::{ConnectAccepted, ConnectionRequest, Network};
use crate::traits::UiAssetProvider;
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

struct AppState {
    backend: Arc<WpaCtrlBackend>,
    status: Arc<StatusPublisher>,
    known_networks: Arc<Mutex<KnownNetworks>>,
    store: NetworkStore,
    scanned_networks: Vec<Network>,
    ui_provider: Arc<dyn UiAssetProvider>,
    connect_in_progress: AtomicBool,
    shutdown: Mutex<Option<oneshot::Sender<()>>>,
}

pub async fn run_server(
    backend: Arc<WpaCtrlBackend>,
    status: Arc<StatusPublisher>,
    known_networks: Arc<Mutex<KnownNetworks>>,
    store: NetworkStore,
    scanned_networks: Vec<Network>,
) -> anyhow::Result<()> {
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let app_state = Arc::new(AppState {
        backend: backend.clone(),
        status,
        known_networks,
        store,
        scanned_networks,
        ui_provider: Arc::new(EmbedFrontend::new()),
        connect_in_progress: AtomicBool::new(false),
        shutdown: Mutex::new(Some(shutdown_tx)),
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
    axum::serve(listener, app.into_make_service())
        .with_graceful_shutdown(async move {
            let _ = shutdown_rx.await;
        })
        .await?;
    Ok(())
}

async fn api_scan(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    (StatusCode::OK, Json(state.scanned_networks.clone()))
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
    if state
        .connect_in_progress
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
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
        let result = state_for_task
            .backend
            .connect_from_provisioning(&payload)
            .await;

        if result.is_ok() {
            {
                let mut guard = state_for_task.known_networks.lock().await;
                guard.upsert_success(&payload);
                if let Err(err) = state_for_task.store.save(&guard).await {
                    tracing::error!("failed to save known networks: {}", err);
                }
            }

            if let Some(sender) = state_for_task.shutdown.lock().await.take() {
                let _ = sender.send(());
            }
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
