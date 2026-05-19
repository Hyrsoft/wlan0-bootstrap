use super::{ConnectedInfo, WpaCtrlBackend};
use crate::networks::KnownNetwork;
use crate::status::WifiState;
use crate::structs::ConnectionRequest;
use anyhow::{Context, Result, anyhow};
use std::time::Duration;

impl WpaCtrlBackend {
    pub async fn connect_known(&self, known: &KnownNetwork) -> Result<ConnectedInfo> {
        let request = ConnectionRequest {
            ssid: known.ssid.clone(),
            password: known.psk.clone(),
        };
        self.status
            .set_state(WifiState::ConnectingKnown, Some(request.ssid.clone()), None)
            .await?;
        self.connect_station(&request).await
    }

    pub async fn monitor_until_disconnected(&self, ssid: &str) {
        // 当前监控策略保持简单：轮询 wpa_supplicant 的 STATUS。
        // 一旦不再是 COMPLETED，就回到主循环重新扫描和决策。
        loop {
            tokio::time::sleep(Duration::from_secs(10)).await;
            let status = match self.send_cmd("STATUS").await {
                Ok(status) => status,
                Err(err) => {
                    tracing::warn!("failed to poll connection status: {}", err);
                    continue;
                }
            };

            if parse_wpa_state(&status) != Some("COMPLETED") {
                let _ = self
                    .status
                    .set_state(WifiState::Reconnecting, Some(ssid.to_string()), None)
                    .await;
                break;
            }
        }
    }

    pub(super) async fn connect_station(
        &self,
        request: &ConnectionRequest,
    ) -> Result<ConnectedInfo> {
        // 连接动作全部通过 wpa_supplicant 控制接口完成。
        // request.password 可能是明文密码，也可能是历史数据中的 64 位 raw PSK。
        tracing::info!("creating wpa_supplicant network: ssid={}", request.ssid);
        let net_id = self.add_network().await?;
        tracing::info!(
            "configuring wpa_supplicant network: ssid={} net_id={}",
            request.ssid,
            net_id
        );
        let result = self.configure_and_enable_network(net_id, request).await;
        if let Err(err) = result {
            tracing::warn!(
                "failed to configure wpa_supplicant network: ssid={} net_id={} error={}",
                request.ssid,
                net_id,
                err
            );
            let _ = self.send_cmd(&format!("REMOVE_NETWORK {}", net_id)).await;
            return Err(err);
        }
        tracing::info!(
            "enabled wpa_supplicant network: ssid={} net_id={}",
            request.ssid,
            net_id
        );

        let connected = match self.wait_for_connection(request, net_id).await {
            Ok(connected) => connected,
            Err(err) => {
                // 关联超时、密码错误或 DHCP 失败后必须释放 STA network。
                // 否则回到 Soft AP 时 wpa_supplicant 仍可能占着接口，hostapd 会启动失败。
                let _ = self.send_cmd("DISCONNECT").await;
                let _ = self.send_cmd(&format!("REMOVE_NETWORK {}", net_id)).await;
                return Err(err);
            }
        };
        if self.config.ownership.wpa_update_config {
            let _ = self.send_cmd("SAVE_CONFIG").await;
        }

        self.status
            .set_state(
                WifiState::Connected,
                Some(request.ssid.clone()),
                connected.ip.clone(),
            )
            .await?;
        Ok(connected)
    }

    async fn add_network(&self) -> Result<u32> {
        let net_id = self.send_cmd("ADD_NETWORK").await?;
        net_id
            .trim()
            .parse::<u32>()
            .context("failed to parse ADD_NETWORK response")
    }

    async fn configure_and_enable_network(
        &self,
        net_id: u32,
        request: &ConnectionRequest,
    ) -> Result<()> {
        let ssid_hex = hex::encode(request.ssid.as_bytes());
        self.send_cmd(&format!("SET_NETWORK {} ssid {}", net_id, ssid_hex))
            .await?;

        if request.password.is_empty() {
            self.send_cmd(&format!("SET_NETWORK {} key_mgmt NONE", net_id))
                .await?;
        } else {
            // 手动验证表明 RK3576/bcmdhd 上显式设置 key_mgmt 更稳定。
            // 只设置 psk 时，部分驱动/固件组合会在 ASSOCIATING 后直接 DISCONNECTED。
            self.send_cmd(&format!("SET_NETWORK {} key_mgmt WPA-PSK", net_id))
                .await?;
            self.send_cmd(&format!(
                "SET_NETWORK {} psk {}",
                net_id,
                format_wpa_psk(&request.password)
            ))
            .await?;
        }

        self.send_cmd(&format!("ENABLE_NETWORK {}", net_id)).await?;
        Ok(())
    }

    async fn wait_for_connection(
        &self,
        request: &ConnectionRequest,
        net_id: u32,
    ) -> Result<ConnectedInfo> {
        let started = tokio::time::Instant::now();
        let timeout = Duration::from_secs(self.config.timeouts.connect_seconds);
        let mut last_state = String::new();
        tracing::info!(
            "waiting for STA association: ssid={} net_id={} timeout={}s",
            request.ssid,
            net_id,
            self.config.timeouts.connect_seconds
        );

        loop {
            if started.elapsed() > timeout {
                tracing::warn!(
                    "STA association timed out: ssid={} net_id={} elapsed={}s",
                    request.ssid,
                    net_id,
                    started.elapsed().as_secs()
                );
                let _ = self.send_cmd(&format!("REMOVE_NETWORK {}", net_id)).await;
                return Err(anyhow!("association_timeout"));
            }

            tokio::time::sleep(Duration::from_secs(2)).await;
            let status = self.send_cmd("STATUS").await?;
            let wpa_state = parse_wpa_state(&status).unwrap_or("UNKNOWN");
            if wpa_state != last_state {
                last_state = wpa_state.to_string();
                tracing::info!(
                    "wpa_supplicant state changed: ssid={} net_id={} state={} bssid={:?} ip={:?}",
                    request.ssid,
                    net_id,
                    wpa_state,
                    wpa_status_field(&status, "bssid"),
                    wpa_status_field(&status, "ip_address")
                );
            } else {
                tracing::debug!(
                    "wpa_supplicant state polling: ssid={} net_id={} state={} elapsed={}s",
                    request.ssid,
                    net_id,
                    wpa_state,
                    started.elapsed().as_secs()
                );
            }

            match Some(wpa_state) {
                Some("COMPLETED") => {
                    tracing::info!(
                        "STA association completed; starting DHCP: ssid={}",
                        request.ssid
                    );
                    let ip = self.run_dhcp().await?;
                    return Ok(ConnectedInfo {
                        ssid: request.ssid.clone(),
                        ip,
                    });
                }
                Some("ASSOCIATING")
                | Some("ASSOCIATED")
                | Some("4WAY_HANDSHAKE")
                | Some("GROUP_HANDSHAKE")
                | Some("SCANNING") => {}
                Some("DISCONNECTED") | Some("INACTIVE") | Some("INTERFACE_DISABLED")
                    if started.elapsed() > Duration::from_secs(5) =>
                {
                    tracing::warn!(
                        "STA connection failed before DHCP: ssid={} net_id={} state={} elapsed={}s",
                        request.ssid,
                        net_id,
                        wpa_state,
                        started.elapsed().as_secs()
                    );
                    let _ = self.send_cmd(&format!("REMOVE_NETWORK {}", net_id)).await;
                    return Err(anyhow!("network_not_found_or_wrong_password"));
                }
                _ => {}
            }
        }
    }
}

fn parse_wpa_state(status: &str) -> Option<&str> {
    wpa_status_field(status, "wpa_state")
}

fn wpa_status_field<'a>(status: &'a str, name: &str) -> Option<&'a str> {
    status.lines().find_map(|line| {
        let (key, value) = line.split_once('=')?;
        (key == name).then_some(value)
    })
}

fn quote_wpa_string(value: &str) -> String {
    let mut quoted = String::with_capacity(value.len() + 2);
    quoted.push('"');
    for ch in value.chars() {
        match ch {
            '\\' => quoted.push_str("\\\\"),
            '"' => quoted.push_str("\\\""),
            _ => quoted.push(ch),
        }
    }
    quoted.push('"');
    quoted
}

fn format_wpa_psk(value: &str) -> String {
    if is_raw_wpa_psk(value) {
        value.to_string()
    } else {
        quote_wpa_string(value)
    }
}

fn is_raw_wpa_psk(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_wpa_psk_quotes_passphrases_but_not_raw_psk() {
        let raw = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

        assert_eq!(format_wpa_psk(raw), raw);
        assert_eq!(format_wpa_psk("plain\"pass"), "\"plain\\\"pass\"");
    }
}
