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
        // 首次启动没有 networks.toml 是正常情况。
        // 只有解析失败或读文件失败才向上返回错误。
        match fs::read_to_string(&self.path).await {
            Ok(content) => toml::from_str(&content)
                .with_context(|| format!("failed to parse {}", self.path.display())),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(KnownNetworks::default()),
            Err(err) => Err(err).with_context(|| format!("failed to read {}", self.path.display())),
        }
    }

    pub async fn save(&self, networks: &KnownNetworks) -> Result<()> {
        // 已知网络写入必须原子替换，避免掉电或进程退出留下半截 TOML。
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
        // 候选网络必须同时满足：未禁用，并且本轮扫描确实看到了 SSID。
        // 排序顺序与蓝图一致：优先级、最近成功时间、当前信号强度。
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
        // 当前阶段按用户要求保存可直接交给 wpa_supplicant 的密码字符串。
        // 字段名仍保留 psk，后续如果恢复 PSK 派生，可以兼容迁移。
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::structs::Network;

    #[test]
    fn candidates_for_scan_orders_by_priority_time_and_signal() {
        let known = KnownNetworks {
            networks: vec![
                KnownNetwork {
                    ssid: "slow".to_string(),
                    security: "wpa-psk".to_string(),
                    psk: "a".to_string(),
                    priority: 1,
                    last_connected_at: 30,
                    disabled: false,
                },
                KnownNetwork {
                    ssid: "preferred".to_string(),
                    security: "wpa-psk".to_string(),
                    psk: "b".to_string(),
                    priority: 10,
                    last_connected_at: 10,
                    disabled: false,
                },
                KnownNetwork {
                    ssid: "disabled".to_string(),
                    security: "wpa-psk".to_string(),
                    psk: "c".to_string(),
                    priority: 100,
                    last_connected_at: 100,
                    disabled: true,
                },
                KnownNetwork {
                    ssid: "recent".to_string(),
                    security: "wpa-psk".to_string(),
                    psk: "d".to_string(),
                    priority: 10,
                    last_connected_at: 20,
                    disabled: false,
                },
            ],
        };
        let scanned = vec![
            Network {
                ssid: "preferred".to_string(),
                signal: 90,
                security: "WPA2".to_string(),
            },
            Network {
                ssid: "recent".to_string(),
                signal: 20,
                security: "WPA2".to_string(),
            },
            Network {
                ssid: "slow".to_string(),
                signal: 100,
                security: "WPA2".to_string(),
            },
            Network {
                ssid: "disabled".to_string(),
                signal: 100,
                security: "WPA2".to_string(),
            },
        ];

        let candidates = known.candidates_for_scan(&scanned);

        assert_eq!(candidates.len(), 3);
        assert_eq!(candidates[0].ssid, "recent");
        assert_eq!(candidates[1].ssid, "preferred");
        assert_eq!(candidates[2].ssid, "slow");
    }

    #[test]
    fn upsert_success_updates_existing_network() {
        let mut known = KnownNetworks::default();
        known.upsert_success(&ConnectionRequest {
            ssid: "Home".to_string(),
            password: "old".to_string(),
        });
        known.upsert_success(&ConnectionRequest {
            ssid: "Home".to_string(),
            password: "new".to_string(),
        });

        assert_eq!(known.networks.len(), 1);
        assert_eq!(known.networks[0].psk, "new");
        assert_eq!(known.networks[0].security, "wpa-psk");
        assert!(!known.networks[0].disabled);
    }
}
