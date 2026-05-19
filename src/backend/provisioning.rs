use super::{ConnectedInfo, WpaCtrlBackend};
use crate::status::{ErrorReason, WifiState};
use crate::structs::ConnectionRequest;
use anyhow::{Result, anyhow};

impl WpaCtrlBackend {
    pub async fn start_provisioning_ap(&self) -> Result<()> {
        // 单射频设备不能假设 AP+STA 并发可用。
        // 进入配网时先切到 AP 流程，由 hostapd/dnsmasq 提供临时网络。
        tracing::info!(
            "starting provisioning AP: interface={} ssid={} bind_addr={} gateway={}",
            self.config.interface.name,
            self.config.ap_ssid(),
            self.config.ap.bind_addr,
            self.config.ap.gateway_cidr
        );
        self.set_provisioning_state(WifiState::ProvisioningApStarting, None, None)
            .await?;
        if let Err(err) = self.start_ap().await {
            let message = err.to_string();
            self.status
                .set_error(ErrorReason::ApStartFailed, message.clone(), None)
                .await?;
            return Err(anyhow!(message));
        }
        self.set_provisioning_state(
            WifiState::ProvisioningApRunning,
            Some(self.config.ap_ssid()),
            Some(self.config.ap.bind_addr.clone()),
        )
        .await?;
        Ok(())
    }

    pub async fn stop_provisioning_ap(&self) -> Result<()> {
        self.stop_ap().await
    }

    async fn set_provisioning_state(
        &self,
        state: WifiState,
        ssid: Option<String>,
        address: Option<String>,
    ) -> Result<()> {
        // 如果刚经历连接失败，恢复 AP 时保留 last_error，方便 Web UI 展示失败原因。
        // 普通进入 AP 时没有 last_error，保留行为不会引入额外状态。
        if self.status.snapshot().await.last_error.is_some() {
            self.status
                .set_state_retaining_error(state, ssid, address)
                .await
        } else {
            self.status.set_state(state, ssid, address).await
        }
    }

    pub async fn connect_from_provisioning(
        &self,
        request: &ConnectionRequest,
    ) -> Result<ConnectedInfo> {
        // 用户提交新 Wi-Fi 后必须停止 Soft AP，再切回 STA 连接目标网络。
        // 如果连接失败，会重新启动 Soft AP，让用户能继续修改密码或选择网络。
        self.status
            .set_state(
                WifiState::ProvisioningConnecting,
                Some(request.ssid.clone()),
                None,
            )
            .await?;
        tracing::info!(
            "switching from provisioning AP to STA connection: target_ssid={}",
            request.ssid
        );
        let _ = self.stop_ap().await;
        if let Err(err) = self.apply_bcmdhd_mode_switch_reset_quirk("ap_to_sta").await {
            // AP->STA 复位属于平台兼容性补丁，失败时继续交给 wpa_supplicant 尝试连接。
            // 这样不会因为一个补丁命令失败而直接丢掉 Web 配网流程的 AP 回退能力。
            tracing::warn!("failed to apply AP->STA platform quirk: {}", err);
        }

        match self.connect_station(request).await {
            Ok(info) => {
                tracing::info!(
                    "STA connection succeeded from provisioning: ssid={} ip={:?}",
                    info.ssid,
                    info.ip
                );
                Ok(info)
            }
            Err(err) => {
                let message = err.to_string();
                tracing::warn!(
                    "STA connection failed from provisioning: ssid={} error={}; refreshing scan cache before restoring AP",
                    request.ssid,
                    message
                );
                self.refresh_scan_cache_before_ap_restore(&request.ssid)
                    .await;
                self.status
                    .set_error(
                        classify_connection_error(&message),
                        message,
                        Some(request.ssid.clone()),
                    )
                    .await?;
                self.start_provisioning_ap().await?;
                Err(err)
            }
        }
    }
}

fn classify_connection_error(message: &str) -> ErrorReason {
    if message.contains("dhcp_failed") || message.contains("dhcp_timeout") {
        ErrorReason::DhcpFailed
    } else if message.contains("association_timeout") {
        ErrorReason::AssociationTimeout
    } else if message.contains("wrong_password") {
        ErrorReason::WrongPassword
    } else if message.contains("network_not_found") {
        ErrorReason::NetworkNotFound
    } else {
        ErrorReason::InternalError
    }
}
