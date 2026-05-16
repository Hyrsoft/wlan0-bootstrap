use crate::structs::ConnectionRequest;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::fs;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct KnownNetwork {
    pub ssid: String,
    pub security: String,
    pub psk: String,
    pub priority: i32,
    pub last_connected_at: u64,
    pub disabled: bool,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct KnownNetworks {
    pub networks: Vec<KnownNetwork>,
}

#[derive(Debug, Clone)]
pub struct NetworkStore {
    path: PathBuf,
}

impl NetworkStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub async fn load(&self) -> Result<KnownNetworks> {
        match fs::read_to_string(&self.path).await {
            Ok(content) => toml::from_str(&content)
                .with_context(|| format!("failed to parse {}", self.path.display())),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(KnownNetworks::default()),
            Err(err) => Err(err).with_context(|| format!("failed to read {}", self.path.display())),
        }
    }

    pub async fn save(&self, networks: &KnownNetworks) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)
                .await
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }

        let content = toml::to_string_pretty(networks).context("failed to serialize networks")?;
        atomic_write(&self.path, content.as_bytes()).await
    }
}

impl KnownNetworks {
    pub fn candidates_for_scan<'a>(
        &'a self,
        scanned: &'a [crate::structs::Network],
    ) -> Vec<&'a KnownNetwork> {
        let mut candidates = self
            .networks
            .iter()
            .filter(|known| !known.disabled)
            .filter(|known| scanned.iter().any(|network| network.ssid == known.ssid))
            .collect::<Vec<_>>();

        candidates.sort_by(|left, right| {
            right
                .priority
                .cmp(&left.priority)
                .then_with(|| right.last_connected_at.cmp(&left.last_connected_at))
                .then_with(|| {
                    signal_for(scanned, &right.ssid).cmp(&signal_for(scanned, &left.ssid))
                })
        });
        candidates
    }

    pub fn upsert_success(&mut self, request: &ConnectionRequest) {
        let now = unix_timestamp();
        if let Some(existing) = self
            .networks
            .iter_mut()
            .find(|network| network.ssid == request.ssid)
        {
            existing.psk = request.password.clone();
            existing.security = if request.password.is_empty() {
                "open".to_string()
            } else {
                "wpa-psk".to_string()
            };
            existing.last_connected_at = now;
            existing.disabled = false;
            return;
        }

        self.networks.push(KnownNetwork {
            ssid: request.ssid.clone(),
            security: if request.password.is_empty() {
                "open".to_string()
            } else {
                "wpa-psk".to_string()
            },
            psk: request.password.clone(),
            priority: 0,
            last_connected_at: now,
            disabled: false,
        });
    }
}

fn signal_for(scanned: &[crate::structs::Network], ssid: &str) -> u8 {
    scanned
        .iter()
        .filter(|network| network.ssid == ssid)
        .map(|network| network.signal)
        .max()
        .unwrap_or(0)
}

fn unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
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
