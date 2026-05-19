use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::fs;
use tokio::io::AsyncWriteExt;
use tokio::net::UnixListener;
use tokio::sync::{RwLock, broadcast};

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
pub enum WifiState {
    Booting,
    Preflight,
    Scanning,
    ConnectingKnown,
    Connected,
    Reconnecting,
    ProvisioningApStarting,
    ProvisioningApRunning,
    ProvisioningConnecting,
    Failed,
    ShuttingDown,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorReason {
    CommandMissing,
    PermissionDenied,
    InterfaceMissing,
    InterfaceBusy,
    ScanFailed,
    NoKnownNetwork,
    NetworkNotFound,
    WrongPassword,
    AssociationTimeout,
    DhcpFailed,
    ApStartFailed,
    StorageFailed,
    InternalError,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct StatusError {
    pub reason: ErrorReason,
    pub message: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct StatusSnapshot {
    pub state: WifiState,
    pub ssid: Option<String>,
    pub address: Option<String>,
    pub last_error: Option<StatusError>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StatusEvent {
    StateChanged {
        state: WifiState,
        ssid: Option<String>,
        address: Option<String>,
    },
    ConnectionFailed {
        ssid: Option<String>,
        reason: ErrorReason,
        message: String,
    },
}

#[derive(Debug)]
pub struct StatusPublisher {
    snapshot: RwLock<StatusSnapshot>,
    status_path: PathBuf,
    event_socket_path: PathBuf,
    events: broadcast::Sender<StatusEvent>,
}

impl StatusPublisher {
    pub async fn new(status_path: PathBuf, event_socket_path: PathBuf) -> Result<Arc<Self>> {
        // 状态快照和事件 socket 都放在 run_dir 下。
        // 快照给轮询型消费者使用，socket 给音频/屏幕/LED 等事件型消费者使用。
        if let Some(parent) = status_path.parent() {
            fs::create_dir_all(parent)
                .await
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        if let Some(parent) = event_socket_path.parent() {
            fs::create_dir_all(parent)
                .await
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }

        let (events, _) = broadcast::channel(64);
        let publisher = Arc::new(Self {
            snapshot: RwLock::new(StatusSnapshot {
                state: WifiState::Booting,
                ssid: None,
                address: None,
                last_error: None,
            }),
            status_path,
            event_socket_path,
            events,
        });
        publisher.write_snapshot().await?;
        Ok(publisher)
    }

    pub async fn start_event_server(self: &Arc<Self>) -> Result<()> {
        // Unix socket 使用 newline-delimited JSON。
        // 新订阅者只接收订阅之后的事件，当前状态请读取 status.json。
        match fs::remove_file(&self.event_socket_path).await {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => {
                return Err(err).with_context(|| {
                    format!("failed to remove {}", self.event_socket_path.display())
                });
            }
        }

        let listener = UnixListener::bind(&self.event_socket_path)
            .with_context(|| format!("failed to bind {}", self.event_socket_path.display()))?;
        let events = self.events.clone();

        tokio::spawn(async move {
            loop {
                let (mut stream, _) = match listener.accept().await {
                    Ok(value) => value,
                    Err(err) => {
                        tracing::warn!("event socket accept failed: {}", err);
                        continue;
                    }
                };
                let mut receiver = events.subscribe();
                tokio::spawn(async move {
                    while let Ok(event) = receiver.recv().await {
                        let line = match serde_json::to_string(&event) {
                            Ok(line) => line,
                            Err(err) => {
                                tracing::warn!("failed to serialize status event: {}", err);
                                continue;
                            }
                        };
                        if stream.write_all(line.as_bytes()).await.is_err() {
                            break;
                        }
                        if stream.write_all(b"\n").await.is_err() {
                            break;
                        }
                    }
                });
            }
        });

        Ok(())
    }

    pub async fn snapshot(&self) -> StatusSnapshot {
        self.snapshot.read().await.clone()
    }

    pub async fn set_state(
        &self,
        state: WifiState,
        ssid: Option<String>,
        address: Option<String>,
    ) -> Result<()> {
        // 状态变化必须先落盘快照，再广播事件。
        // 这样外部程序错过事件时仍能从 status.json 恢复当前状态。
        self.set_state_inner(state, ssid, address, true).await
    }

    pub async fn set_state_retaining_error(
        &self,
        state: WifiState,
        ssid: Option<String>,
        address: Option<String>,
    ) -> Result<()> {
        // 失败后恢复 Soft AP 时保留 last_error。
        // Web UI 通过轮询 status.json 展示失败原因，不能只依赖瞬时事件。
        self.set_state_inner(state, ssid, address, false).await
    }

    async fn set_state_inner(
        &self,
        state: WifiState,
        ssid: Option<String>,
        address: Option<String>,
        clear_error: bool,
    ) -> Result<()> {
        {
            let mut snapshot = self.snapshot.write().await;
            snapshot.state = state;
            snapshot.ssid = ssid.clone();
            snapshot.address = address.clone();
            if clear_error {
                snapshot.last_error = None;
            }
        }
        self.write_snapshot().await?;
        let _ = self.events.send(StatusEvent::StateChanged {
            state,
            ssid,
            address,
        });
        Ok(())
    }

    pub async fn set_error(
        &self,
        reason: ErrorReason,
        message: impl Into<String>,
        ssid: Option<String>,
    ) -> Result<()> {
        // 错误也统一表现为状态快照和事件，UI 与外部提示程序使用同一份事实来源。
        let message = message.into();
        {
            let mut snapshot = self.snapshot.write().await;
            snapshot.state = WifiState::Failed;
            snapshot.ssid = ssid.clone();
            snapshot.address = None;
            snapshot.last_error = Some(StatusError {
                reason: reason.clone(),
                message: message.clone(),
            });
        }
        self.write_snapshot().await?;
        let _ = self.events.send(StatusEvent::ConnectionFailed {
            ssid,
            reason,
            message,
        });
        Ok(())
    }

    async fn write_snapshot(&self) -> Result<()> {
        let snapshot = self.snapshot.read().await.clone();
        let bytes = serde_json::to_vec_pretty(&snapshot).context("failed to serialize status")?;
        atomic_write(&self.status_path, &bytes).await
    }
}

async fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let temp_path = path.with_extension("tmp");
    fs::write(&temp_path, bytes)
        .await
        .with_context(|| format!("failed to write {}", temp_path.display()))?;
    fs::rename(&temp_path, path)
        .await
        .with_context(|| format!("failed to replace {}", path.display()))?;
    Ok(())
}
