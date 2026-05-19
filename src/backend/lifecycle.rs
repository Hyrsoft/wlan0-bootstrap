use super::WpaCtrlBackend;
use crate::status::{ErrorReason, WifiState};
use anyhow::{Context, Result, anyhow};
use std::env;
use std::path::{Path, PathBuf};
use tokio::fs;

impl WpaCtrlBackend {
    pub async fn shutdown(&self) {
        // 进程因错误退出或被上层要求停止时，必须清理本程序启动的系统工具。
        // 否则 Buildroot 设备上会留下孤儿 wpa_supplicant/hostapd/dnsmasq，影响下一次配网。
        self.shutdown_discovery().await;
        let _ = self.stop_ap().await;

        let had_wpa_supplicant = if let Some(mut child) = self.wpa_supplicant.lock().await.take() {
            let _ = child.kill().await;
            true
        } else {
            false
        };

        if had_wpa_supplicant {
            let _ = self.flush_interface_ipv4().await;
            self.remove_existing_wpa_socket().await;
        }
    }

    pub async fn prepare(&self) -> Result<()> {
        // 启动阶段只做当前进程必须拥有的准备工作：
        // 目录、命令、网卡、wpa_supplicant 控制 socket。
        // 不在这里清理其他网络管理器，避免误杀产品系统上的外部进程。
        self.status
            .set_state(WifiState::Preflight, None, None)
            .await?;
        self.preflight().await?;
        self.collect_device_profile().await?;
        self.ensure_station_daemon().await?;
        Ok(())
    }

    pub async fn shutdown_discovery(&self) {
        let mut mdns = self.mdns.lock().await;
        let hostname = mdns.published_hostname();
        mdns.shutdown().await;
        if hostname.is_some() {
            let _ = self.status.set_mdns_stopped(hostname).await;
        }
    }

    async fn preflight(&self) -> Result<()> {
        fs::create_dir_all(&self.config.paths.run_dir)
            .await
            .with_context(|| format!("failed to create {}", self.config.paths.run_dir.display()))?;
        fs::create_dir_all(&self.config.paths.data_dir)
            .await
            .with_context(|| {
                format!("failed to create {}", self.config.paths.data_dir.display())
            })?;

        for command in [
            &self.config.commands.wpa_supplicant,
            &self.config.commands.hostapd,
            &self.config.commands.dnsmasq,
            &self.config.commands.ip,
            &self.config.commands.udhcpc,
        ] {
            if !command_exists(command) {
                self.status
                    .set_error(
                        ErrorReason::CommandMissing,
                        format!("required command not found: {}", command),
                        None,
                    )
                    .await?;
                return Err(anyhow!("required command not found: {}", command));
            }
        }

        let iface_path = Path::new("/sys/class/net").join(&self.config.interface.name);
        if !iface_path.exists() {
            self.status
                .set_error(
                    ErrorReason::InterfaceMissing,
                    format!("interface {} not found", self.config.interface.name),
                    None,
                )
                .await?;
            return Err(anyhow!(
                "interface {} not found",
                self.config.interface.name
            ));
        }

        Ok(())
    }

    async fn collect_device_profile(&self) -> Result<()> {
        let profile =
            crate::device_profile::DeviceProfile::collect(&self.config.interface.name).await;
        tracing::info!(
            "device profile: board={:?} compatible={:?} interface={} driver={:?} bus={:?} quirks={:?}",
            profile.board_model,
            profile.compatible,
            profile.interface.name,
            profile.interface.driver,
            profile.interface.bus,
            profile.quirks
        );
        self.status.set_device_profile(profile.clone()).await?;
        *self.device_profile.write().await = Some(profile);
        Ok(())
    }
}

fn command_exists(command: &str) -> bool {
    let path = Path::new(command);
    if path.components().count() > 1 {
        return path.exists();
    }

    env::var_os("PATH")
        .into_iter()
        .flat_map(|paths| env::split_paths(&paths).collect::<Vec<PathBuf>>())
        .any(|dir| dir.join(command).exists())
}
