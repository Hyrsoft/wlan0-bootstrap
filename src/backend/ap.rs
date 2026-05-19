use super::WpaCtrlBackend;
use anyhow::{Context, Result, anyhow};
use std::time::Duration;
use tokio::fs;
use tokio::process::{Child, Command};

impl WpaCtrlBackend {
    pub(super) async fn start_ap(&self) -> Result<()> {
        // AP/DHCP 仍然调用系统 hostapd 和 dnsmasq。
        // 这里不做内置 AP 或 DHCP server，实现边界保持清晰。
        tracing::info!("preparing AP services on {}", self.config.interface.name);
        let _ = self.stop_ap().await;
        self.flush_interface_ipv4().await?;
        self.apply_bcmdhd_mode_switch_reset_quirk("to_ap").await?;

        let output = Command::new(&self.config.commands.ip)
            .arg("addr")
            .arg("add")
            .arg(&self.config.ap.gateway_cidr)
            .arg("dev")
            .arg(&self.config.interface.name)
            .output()
            .await
            .context("failed to assign AP address")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if !stderr.contains("File exists") {
                return Err(anyhow!("failed to assign AP address: {}", stderr));
            }
        }
        tracing::info!(
            "assigned AP address {} to {}",
            self.config.ap.gateway_cidr,
            self.config.interface.name
        );

        // 这里的 wpa_passphrase 是 hostapd 配置项，表示 Soft AP 的接入密码；
        // 它不是 wpa_passphrase 命令，也不参与 STA 已知网络密码的持久化。
        let hostapd_conf = format!(
            "interface={}\nssid={}\nwpa={}\nwpa_passphrase={}\nhw_mode={}\nchannel={}\nwpa_key_mgmt={}\nwpa_pairwise={}\nrsn_pairwise={}\n",
            self.config.interface.name,
            self.config.ap_ssid(),
            self.config.ap.wpa,
            self.config.ap.password,
            self.config.ap.hw_mode,
            self.config.ap.channel,
            self.config.ap.wpa_key_mgmt,
            self.config.ap.wpa_pairwise,
            self.config.ap.rsn_pairwise
        );
        fs::write(&self.config.paths.hostapd_config, hostapd_conf.as_bytes())
            .await
            .with_context(|| {
                format!(
                    "failed to write {}",
                    self.config.paths.hostapd_config.display()
                )
            })?;

        let hostapd = Command::new(&self.config.commands.hostapd)
            .arg(&self.config.paths.hostapd_config)
            .spawn()
            .context("failed to start hostapd")?;
        *self.hostapd.lock().await = Some(hostapd);
        tokio::time::sleep(Duration::from_secs(1)).await;
        self.ensure_child_running(&self.hostapd, "hostapd").await?;
        tracing::info!(
            "hostapd started: interface={} ssid={} channel={}",
            self.config.interface.name,
            self.config.ap_ssid(),
            self.config.ap.channel
        );

        let ap_ip = self
            .config
            .ap
            .gateway_cidr
            .split_once('/')
            .map(|(ip, _)| ip)
            .unwrap_or(self.config.ap.gateway_cidr.as_str());
        let dnsmasq = Command::new(&self.config.commands.dnsmasq)
            // 目标设备可能已经在 usb0 等调试网口上运行 DHCP 服务。
            // dnsmasq 必须限制在本程序管理的 Wi-Fi 接口和 AP 网关地址上，
            // 避免绑定 0.0.0.0:67 时和系统已有 DHCP 服务冲突。
            .arg(format!("--interface={}", self.config.interface.name))
            .arg("--bind-interfaces")
            .arg(format!("--listen-address={}", ap_ip))
            .arg(format!(
                "--dhcp-range={}",
                format_dnsmasq_dhcp_range(&self.config.interface.name, &self.config.ap.dhcp_range)
            ))
            .arg(format!("--address=/#/{}", ap_ip))
            .arg("--no-resolv")
            .arg("--no-hosts")
            .arg("--no-daemon")
            .spawn()
            .context("failed to start dnsmasq")?;
        *self.dnsmasq.lock().await = Some(dnsmasq);
        tokio::time::sleep(Duration::from_millis(300)).await;
        self.ensure_child_running(&self.dnsmasq, "dnsmasq").await?;
        tracing::info!(
            "dnsmasq started: interface={} listen_address={} dhcp_range={}",
            self.config.interface.name,
            ap_ip,
            format_dnsmasq_dhcp_range(&self.config.interface.name, &self.config.ap.dhcp_range)
        );

        Ok(())
    }

    pub(super) async fn apply_bcmdhd_mode_switch_reset_quirk(&self, context: &str) -> Result<()> {
        if !self.config.platform.auto_driver_quirks {
            return Ok(());
        }

        let profile = self.device_profile.read().await.clone();
        let Some(profile) = profile else {
            return Ok(());
        };

        if !profile.has_quirk("rockchip_bcmdhd_ap_mode_reset") {
            return Ok(());
        }

        // RK + Broadcom bcmdhd 在 AP/STA 模式切换中会保留部分固件状态。
        // 进入 AP 前不复位，hostapd 第二次启动可能报 beacon/security 参数错误；
        // 从 AP 切回 STA 前不复位，wpa_supplicant 可能在 ASSOCIATING 后被 AP 拒绝。
        // 这里仅对自动识别出的 bcmdhd 设备做接口 down/up 复位，不影响其他平台。
        tracing::info!(
            "applying platform quirk rockchip_bcmdhd_ap_mode_reset on {}: context={}",
            self.config.interface.name,
            context
        );
        self.set_interface_down().await?;
        tokio::time::sleep(Duration::from_millis(
            self.config.platform.ap_mode_reset_delay_ms,
        ))
        .await;
        self.set_interface_up().await?;
        tokio::time::sleep(Duration::from_millis(
            self.config.platform.ap_mode_reset_delay_ms,
        ))
        .await;
        Ok(())
    }

    pub(super) async fn flush_interface_ipv4(&self) -> Result<()> {
        // 进入 Soft AP 前清理 STA 阶段遗留的 IPv4 地址。
        // 单射频 TDM 下同一接口不应同时保留上游 Wi-Fi 地址和 AP 网关地址。
        let output = Command::new(&self.config.commands.ip)
            .arg("-4")
            .arg("addr")
            .arg("flush")
            .arg("dev")
            .arg(&self.config.interface.name)
            .output()
            .await
            .context("failed to flush interface IPv4 addresses")?;

        if output.status.success() {
            return Ok(());
        }

        Err(anyhow!(
            "failed to flush interface IPv4 addresses: {}",
            String::from_utf8_lossy(&output.stderr)
        ))
    }

    pub(super) async fn set_interface_up(&self) -> Result<()> {
        let output = Command::new(&self.config.commands.ip)
            .arg("link")
            .arg("set")
            .arg(&self.config.interface.name)
            .arg("up")
            .output()
            .await
            .context("failed to run ip link set up")?;

        if output.status.success() {
            return Ok(());
        }

        Err(anyhow!(
            "failed to set interface up: {}",
            String::from_utf8_lossy(&output.stderr)
        ))
    }

    async fn set_interface_down(&self) -> Result<()> {
        let output = Command::new(&self.config.commands.ip)
            .arg("link")
            .arg("set")
            .arg(&self.config.interface.name)
            .arg("down")
            .output()
            .await
            .context("failed to run ip link set down")?;

        if output.status.success() {
            return Ok(());
        }

        Err(anyhow!(
            "failed to set interface down: {}",
            String::from_utf8_lossy(&output.stderr)
        ))
    }

    async fn ensure_child_running(
        &self,
        child_slot: &tokio::sync::Mutex<Option<Child>>,
        name: &str,
    ) -> Result<()> {
        let mut guard = child_slot.lock().await;
        let Some(child) = guard.as_mut() else {
            return Err(anyhow!("{} process was not started", name));
        };

        match child.try_wait() {
            Ok(Some(status)) => {
                drop(guard);
                let _ = self.stop_ap().await;
                Err(anyhow!(
                    "{} exited immediately with status {}",
                    name,
                    status
                ))
            }
            Ok(None) => Ok(()),
            Err(err) => {
                drop(guard);
                let _ = self.stop_ap().await;
                Err(anyhow!("failed to check {} process status: {}", name, err))
            }
        }
    }

    pub(super) async fn stop_ap(&self) -> Result<()> {
        if let Some(mut child) = self.dnsmasq.lock().await.take() {
            tracing::info!("stopping dnsmasq for provisioning AP");
            let _ = child.kill().await;
        }
        if let Some(mut child) = self.hostapd.lock().await.take() {
            tracing::info!("stopping hostapd for provisioning AP");
            let _ = child.kill().await;
        }

        let output = Command::new(&self.config.commands.ip)
            .arg("addr")
            .arg("del")
            .arg(&self.config.ap.gateway_cidr)
            .arg("dev")
            .arg(&self.config.interface.name)
            .output()
            .await
            .context("failed to remove AP address")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if !stderr.contains("Cannot assign requested address") {
                return Err(anyhow!("failed to remove AP address: {}", stderr));
            }
        } else {
            tracing::info!(
                "removed AP address {} from {}",
                self.config.ap.gateway_cidr,
                self.config.interface.name
            );
        }

        match fs::remove_file(&self.config.paths.hostapd_config).await {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => tracing::warn!(
                "failed to remove {}: {}",
                self.config.paths.hostapd_config.display(),
                err
            ),
        }

        Ok(())
    }
}

fn format_dnsmasq_dhcp_range(iface: &str, range: &str) -> String {
    if range.starts_with("interface:") {
        range.to_string()
    } else {
        format!("interface:{},{}", iface, range)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_dnsmasq_dhcp_range_binds_range_to_interface() {
        assert_eq!(
            format_dnsmasq_dhcp_range("wlan0", "192.168.4.100,192.168.4.200,12h"),
            "interface:wlan0,192.168.4.100,192.168.4.200,12h"
        );
        assert_eq!(
            format_dnsmasq_dhcp_range("wlan0", "interface:wlan1,10.0.0.10,10.0.0.20,1h"),
            "interface:wlan1,10.0.0.10,10.0.0.20,1h"
        );
    }
}
