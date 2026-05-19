use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::Path;
use tokio::fs;
use tokio::process::Command;

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct DeviceProfile {
    pub board_model: Option<String>,
    pub compatible: Vec<String>,
    pub os: Option<String>,
    pub kernel: Option<String>,
    pub interface: InterfaceProfile,
    pub quirks: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct InterfaceProfile {
    pub name: String,
    pub mac: Option<String>,
    pub driver: Option<String>,
    pub device_path: Option<String>,
    pub phy_path: Option<String>,
    pub bus: Option<String>,
    pub modules: Vec<String>,
}

impl DeviceProfile {
    pub async fn collect(interface: &str) -> Self {
        // 设备画像只读取系统状态，不修改接口。
        // 后续平台补丁必须基于这里采集到的板级 compatible、驱动和总线信息做保守判断。
        let compatible = read_device_tree_compatible().await.unwrap_or_default();
        let board_model = read_trimmed("/proc/device-tree/model").await.ok();
        let os = read_os_release().await.ok();
        let kernel = read_command_line("uname", &["-a"]).await.ok();
        let interface_profile = InterfaceProfile::collect(interface).await;

        let mut profile = Self {
            board_model,
            compatible,
            os,
            kernel,
            interface: interface_profile,
            quirks: Vec::new(),
        };
        profile.quirks = profile.detect_quirks();
        profile
    }

    pub fn has_quirk(&self, quirk: &str) -> bool {
        self.quirks.iter().any(|item| item == quirk)
    }

    fn detect_quirks(&self) -> Vec<String> {
        let is_rockchip = self
            .compatible
            .iter()
            .any(|item| item.contains("rockchip,"))
            || self
                .os
                .as_deref()
                .is_some_and(|os| os.to_ascii_lowercase().contains("rockchip"))
            || self
                .kernel
                .as_deref()
                .is_some_and(|kernel| kernel.to_ascii_lowercase().contains("rk"));
        let is_bcmdhd = self
            .interface
            .driver
            .as_deref()
            .is_some_and(|driver| driver == "bcmsdh_sdmmc" || driver.contains("bcmdhd"))
            || self
                .interface
                .modules
                .iter()
                .any(|module| module == "bcmdhd" || module == "dhd_static_buf");

        if is_rockchip && is_bcmdhd {
            vec!["rockchip_bcmdhd_ap_mode_reset".to_string()]
        } else {
            Vec::new()
        }
    }
}

impl InterfaceProfile {
    async fn collect(interface: &str) -> Self {
        let base = format!("/sys/class/net/{interface}");
        let mac = read_trimmed(format!("{base}/address")).await.ok();
        let driver = read_link_basename(format!("{base}/device/driver"))
            .await
            .ok();
        let device_path = canonicalize_to_string(format!("{base}/device")).await.ok();
        let phy_path = canonicalize_to_string(format!("{base}/phy80211"))
            .await
            .ok();
        let bus = device_path.as_deref().and_then(infer_bus);
        let modules = read_kernel_modules().await.unwrap_or_default();

        Self {
            name: interface.to_string(),
            mac,
            driver,
            device_path,
            phy_path,
            bus,
            modules,
        }
    }
}

async fn read_trimmed(path: impl AsRef<Path>) -> Result<String> {
    Ok(fs::read_to_string(path)
        .await?
        .trim_matches(char::from(0))
        .trim()
        .to_string())
}

async fn read_device_tree_compatible() -> Result<Vec<String>> {
    let bytes = fs::read("/proc/device-tree/compatible").await?;
    Ok(bytes
        .split(|byte| *byte == 0)
        .filter_map(|part| std::str::from_utf8(part).ok())
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(ToOwned::to_owned)
        .collect())
}

async fn read_os_release() -> Result<String> {
    let content = fs::read_to_string("/etc/os-release").await?;
    Ok(content
        .lines()
        .find_map(|line| line.strip_prefix("PRETTY_NAME="))
        .map(|value| value.trim_matches('"').to_string())
        .unwrap_or(content))
}

async fn read_command_line(program: &str, args: &[&str]) -> Result<String> {
    let output = Command::new(program).args(args).output().await?;
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

async fn read_link_basename(path: impl AsRef<Path>) -> Result<String> {
    let link = fs::read_link(path).await?;
    Ok(link
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_string())
}

async fn canonicalize_to_string(path: impl AsRef<Path>) -> Result<String> {
    Ok(fs::canonicalize(path).await?.display().to_string())
}

async fn read_kernel_modules() -> Result<Vec<String>> {
    let content = fs::read_to_string("/proc/modules").await?;
    Ok(content
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .map(ToOwned::to_owned)
        .collect())
}

fn infer_bus(path: &str) -> Option<String> {
    if path.contains("/mmc") || path.contains("/sdio") {
        Some("sdio".to_string())
    } else if path.contains("/usb") {
        Some("usb".to_string())
    } else if path.contains("/pci") {
        Some("pci".to_string())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_rockchip_bcmdhd_quirk() {
        let profile = DeviceProfile {
            compatible: vec!["rockchip,rk3576".to_string()],
            interface: InterfaceProfile {
                driver: Some("bcmsdh_sdmmc".to_string()),
                modules: vec!["bcmdhd".to_string()],
                ..InterfaceProfile::default()
            },
            ..DeviceProfile::default()
        };

        assert_eq!(
            profile.detect_quirks(),
            vec!["rockchip_bcmdhd_ap_mode_reset".to_string()]
        );
    }

    #[test]
    fn infer_bus_from_sysfs_path() {
        assert_eq!(
            infer_bus("/sys/devices/platform/mmc_host/mmc2/mmc2:0001:2").as_deref(),
            Some("sdio")
        );
    }
}
