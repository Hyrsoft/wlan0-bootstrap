use super::WpaCtrlBackend;
use crate::status::{ErrorReason, WifiState};
use crate::structs::Network;
use anyhow::Result;
use std::time::Duration;

impl WpaCtrlBackend {
    pub async fn scan(&self) -> Result<Vec<Network>> {
        // 扫描仍通过 wpa_supplicant 控制接口完成。
        // 这里缓存的是进入 Soft AP 前的 STA 扫描结果，供配网页展示。
        self.status
            .set_state(WifiState::Scanning, None, None)
            .await?;
        let mut networks = Vec::new();
        let mut last_error = None;
        for attempt in 1..=3 {
            tracing::info!("Scanning Wi-Fi networks, attempt {}", attempt);
            match self.scan_once().await {
                Ok(found) => {
                    networks = found;
                    if !networks.is_empty() {
                        break;
                    }
                }
                Err(err) => {
                    tracing::warn!("scan attempt {} failed: {}", attempt, err);
                    last_error = Some(err.to_string());
                }
            }

            if attempt < 3 {
                tokio::time::sleep(Duration::from_secs(self.config.timeouts.scan_seconds)).await;
            }
        }

        if networks.is_empty() {
            let message = last_error.map_or_else(
                || "scan returned no networks".to_string(),
                |err| format!("scan failed after retries: {}", err),
            );
            self.status
                .set_error(ErrorReason::ScanFailed, message, None)
                .await?;
        }

        self.replace_scan_cache(networks.clone()).await;
        Ok(networks)
    }

    pub async fn cached_networks(&self) -> Vec<Network> {
        self.scan_cache.read().await.clone()
    }

    async fn replace_scan_cache(&self, networks: Vec<Network>) {
        tracing::info!("updated Wi-Fi scan cache: networks={}", networks.len());
        *self.scan_cache.write().await = networks;
    }

    pub(super) async fn refresh_scan_cache_before_ap_restore(&self, ssid: &str) {
        // 连接失败后仍处在 STA/wpa_supplicant 控制路径上，此时还没有重新启动 hostapd。
        // 在这里额外扫描一次，可以让恢复 AP 后的 /api/scan 返回更新过的 Wi-Fi 列表。
        tracing::info!(
            "refreshing Wi-Fi scan cache before provisioning AP restore: failed_ssid={}",
            ssid
        );
        match self.scan_once().await {
            Ok(networks) => {
                let count = networks.len();
                self.replace_scan_cache(networks).await;
                tracing::info!(
                    "refreshed Wi-Fi scan cache before provisioning AP restore: networks={}",
                    count
                );
            }
            Err(err) => {
                // 扫描失败不能阻断 AP 回退，否则用户会失去继续配网的入口。
                tracing::warn!(
                    "failed to refresh Wi-Fi scan cache before provisioning AP restore: ssid={} error={}",
                    ssid,
                    err
                );
            }
        }
    }

    async fn scan_once(&self) -> Result<Vec<Network>> {
        self.send_cmd("SCAN").await?;
        tokio::time::sleep(Duration::from_secs(self.config.timeouts.scan_seconds)).await;
        let output = self.send_cmd("SCAN_RESULTS").await?;
        Ok(parse_scan_results(&output))
    }
}

fn parse_scan_results(output: &str) -> Vec<Network> {
    let mut networks: Vec<Network> = Vec::new();
    for line in output.lines().skip(1) {
        let parts = line.split('\t').collect::<Vec<_>>();
        if parts.len() < 5 {
            continue;
        }

        let signal_dbm = parts[2].parse::<i16>().unwrap_or(-100);
        let ssid = String::from_utf8_lossy(&unescape_wpa_ssid(parts[4])).to_string();
        if ssid.is_empty() {
            continue;
        }

        let security = if parts[3].contains("WPA2") {
            "WPA2"
        } else if parts[3].contains("WPA") {
            "WPA"
        } else {
            "Open"
        }
        .to_string();

        let signal = ((signal_dbm.clamp(-100, -50) + 100) * 2) as u8;
        if let Some(existing) = networks.iter_mut().find(|network| network.ssid == ssid) {
            if signal > existing.signal {
                existing.signal = signal;
            }
            continue;
        }
        networks.push(Network {
            ssid,
            signal,
            security,
        });
    }
    networks
}

fn unescape_wpa_ssid(s: &str) -> Vec<u8> {
    fn hex_val(byte: u8) -> Option<u8> {
        match byte {
            b'0'..=b'9' => Some(byte - b'0'),
            b'a'..=b'f' => Some(10 + byte - b'a'),
            b'A'..=b'F' => Some(10 + byte - b'A'),
            _ => None,
        }
    }

    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'\\'
            && index + 3 < bytes.len()
            && bytes[index + 1] == b'x'
            && let (Some(left), Some(right)) =
                (hex_val(bytes[index + 2]), hex_val(bytes[index + 3]))
        {
            out.push((left << 4) | right);
            index += 4;
            continue;
        }
        out.push(bytes[index]);
        index += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_scan_results_deduplicates_and_decodes_ssid() {
        let output = "\
bssid / frequency / signal level / flags / ssid
00:11:22:33:44:55\t2412\t-40\t[WPA2-PSK-CCMP][ESS]\tHome\\x20WiFi
00:11:22:33:44:66\t2412\t-80\t[WPA2-PSK-CCMP][ESS]\tHome\\x20WiFi
00:11:22:33:44:77\t2462\t-70\t[ESS]\tCafe
";

        let networks = parse_scan_results(output);

        assert_eq!(networks.len(), 2);
        assert_eq!(networks[0].ssid, "Home WiFi");
        assert_eq!(networks[0].security, "WPA2");
        assert_eq!(networks[0].signal, 100);
        assert_eq!(networks[1].ssid, "Cafe");
        assert_eq!(networks[1].security, "Open");
    }
}
