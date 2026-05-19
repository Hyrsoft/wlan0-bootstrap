use super::WpaCtrlBackend;
use anyhow::{Context, Result, anyhow};
use std::process::Stdio;
use std::time::Duration;
use tokio::fs;
use tokio::process::Command;
use wpa_ctrl::WpaControllerBuilder;

impl WpaCtrlBackend {
    pub(super) async fn ensure_station_daemon(&self) -> Result<()> {
        // 默认不接管已有 wpa_supplicant。
        // 如果 socket 已存在，认为接口可能被外部进程管理，直接失败并发布状态。
        self.check_existing_interface_owner().await?;
        self.check_existing_wpa_socket().await?;
        self.write_wpa_config().await?;
        if self.config.ownership.force_takeover {
            self.remove_existing_wpa_socket().await;
        }
        self.set_interface_up().await?;

        let child = Command::new(&self.config.commands.wpa_supplicant)
            .arg("-i")
            .arg(&self.config.interface.name)
            .arg("-c")
            .arg(&self.config.paths.wpa_config)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .context("failed to start wpa_supplicant")?;
        *self.wpa_supplicant.lock().await = Some(child);

        tokio::time::sleep(Duration::from_secs(2)).await;

        let controller = WpaControllerBuilder::new()
            .open(&self.config.interface.name)
            .context("failed to connect wpa_supplicant control socket")?;
        *self
            .cmd_ctrl
            .lock()
            .map_err(|_| anyhow!("wpa controller lock poisoned"))? = Some(controller);
        Ok(())
    }

    async fn check_existing_interface_owner(&self) -> Result<()> {
        // 真实设备上不一定会留下 /run/wpa_supplicant/wlan0。
        // 例如 Luckfox Buildroot 镜像会通过 rkwifi_server 和 /data/wpa_supplicant.conf
        // 先启动自己的 wpa_supplicant；这类 owner 必须在启动本程序前拦截。
        if self.config.ownership.force_takeover {
            return Ok(());
        }

        let output = match Command::new("ps").arg("-ef").output().await {
            Ok(output) => output,
            Err(err) => {
                tracing::warn!(
                    "failed to inspect process list for interface owner: {}",
                    err
                );
                return Ok(());
            }
        };

        if !output.status.success() {
            tracing::warn!(
                "ps -ef failed while checking interface owner: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            return Ok(());
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        if let Some(owner) = find_interface_owner(&stdout, &self.config.interface.name) {
            let message = format!(
                "interface {} appears to be managed by another process: {}; stop the existing owner or set ownership.force_takeover=true only for explicit takeover",
                self.config.interface.name, owner
            );
            self.status
                .set_error(
                    crate::status::ErrorReason::InterfaceBusy,
                    message.clone(),
                    None,
                )
                .await?;
            return Err(anyhow!(message));
        }

        Ok(())
    }

    async fn check_existing_wpa_socket(&self) -> Result<()> {
        // 这里只检查控制 socket，属于保守策略。
        // 后续可结合 pidfile 或进程命令行进一步区分 stale socket 和外部 owner。
        let socket_path = self.config.paths.wpa_ctrl.join(&self.config.interface.name);
        if !socket_path.exists() || self.config.ownership.force_takeover {
            return Ok(());
        }

        let message = format!(
            "wpa_supplicant control socket already exists at {}; set ownership.force_takeover=true only when this daemon may take over {}",
            socket_path.display(),
            self.config.interface.name
        );
        self.status
            .set_error(
                crate::status::ErrorReason::InterfaceBusy,
                message.clone(),
                None,
            )
            .await?;
        Err(anyhow!(message))
    }

    async fn write_wpa_config(&self) -> Result<()> {
        // wpa_supplicant.conf 只作为运行期控制入口，不作为已知网络数据库。
        // 已知网络由 networks.toml 维护，避免让 SAVE_CONFIG 改写部署配置。
        if let Some(parent) = self.config.paths.wpa_config.parent() {
            fs::create_dir_all(parent)
                .await
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        fs::create_dir_all(&self.config.paths.wpa_ctrl)
            .await
            .with_context(|| {
                format!("failed to create {}", self.config.paths.wpa_ctrl.display())
            })?;
        if let Some(parent) = self.config.paths.wpa_ctrl.parent() {
            fs::create_dir_all(parent)
                .await
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }

        let update_config = u8::from(self.config.ownership.wpa_update_config);
        let content = format!(
            "ctrl_interface=DIR={} GROUP={}\nupdate_config={}\n",
            self.config.paths.wpa_ctrl.display(),
            self.config.ownership.wpa_group,
            update_config
        );
        fs::write(&self.config.paths.wpa_config, content.as_bytes())
            .await
            .with_context(|| {
                format!("failed to write {}", self.config.paths.wpa_config.display())
            })?;
        Ok(())
    }

    pub(super) async fn remove_existing_wpa_socket(&self) {
        let socket_path = self.config.paths.wpa_ctrl.join(&self.config.interface.name);
        match fs::remove_file(&socket_path).await {
            Ok(()) => tracing::debug!("removed existing socket {}", socket_path.display()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => tracing::warn!("failed to remove {}: {}", socket_path.display(), err),
        }
    }

    pub(super) async fn send_cmd(&self, cmd: &str) -> Result<String> {
        // wpa-ctrl crate 的 request/recv 是阻塞接口。
        // 放到 spawn_blocking 中执行，避免占用 tokio worker 线程。
        {
            let ctrl_guard = self
                .cmd_ctrl
                .lock()
                .map_err(|_| anyhow!("wpa controller lock poisoned"))?;
            if ctrl_guard.is_none() {
                return Err(anyhow!("wpa controller not available"));
            }
        }

        let cmd = cmd.to_string();
        let ctrl = self.cmd_ctrl.clone();
        tokio::task::spawn_blocking(move || {
            let mut guard = ctrl
                .lock()
                .map_err(|_| anyhow!("wpa controller lock poisoned"))?;
            let controller = guard
                .as_mut()
                .ok_or_else(|| anyhow!("wpa controller not available"))?;

            use wpa_ctrl::WpaControlReq;
            controller
                .request(WpaControlReq::raw(&cmd))
                .map_err(|err| anyhow!("wpa_ctrl request failed: {}", err))?;

            loop {
                match controller.recv() {
                    Ok(Some(message)) if message.is_unsolicited() => continue,
                    Ok(Some(message)) if message.as_fail().is_some() => {
                        return Err(anyhow!("WPA command failed: {}", message.raw));
                    }
                    Ok(Some(message)) => return Ok(message.raw.to_string()),
                    Ok(None) => return Err(anyhow!("no response received from wpa_supplicant")),
                    Err(err) => return Err(anyhow!("failed to receive wpa response: {}", err)),
                }
            }
        })
        .await
        .context("wpa command worker failed")?
    }
}

fn find_interface_owner<'a>(process_list: &'a str, iface: &str) -> Option<&'a str> {
    process_list.lines().find(|line| {
        is_wpa_supplicant_for_iface(line, iface)
            || line.contains("rkwifi_server")
            || line.contains("NetworkManager")
            || line.contains("connmand")
    })
}

fn is_wpa_supplicant_for_iface(line: &str, iface: &str) -> bool {
    line.contains("wpa_supplicant")
        && (line.contains(&format!("-i {}", iface))
            || line.contains(&format!("-i{}", iface))
            || line.contains(&format!("--interface {}", iface))
            || line.contains(&format!("--interface={}", iface)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_interface_owner_detects_common_wlan0_owners() {
        let processes = "\
root 480 1 0 ? 00:00:00 rkwifi_server start
root 576 1 0 ? 00:00:00 wpa_supplicant -B -i wlan0 -c /data/wpa_supplicant.conf -d
";

        let owner = find_interface_owner(processes, "wlan0").expect("owner should be detected");

        assert!(owner.contains("rkwifi_server") || owner.contains("wpa_supplicant"));
    }

    #[test]
    fn find_interface_owner_ignores_other_interfaces() {
        let processes = "root 576 1 0 ? 00:00:00 wpa_supplicant -B -i wlan1 -c /data/wpa.conf\n";

        assert!(find_interface_owner(processes, "wlan0").is_none());
    }
}
