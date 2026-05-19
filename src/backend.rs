use crate::config::AppConfig;
use crate::device_profile::DeviceProfile;
use crate::networks::KnownNetwork;
use crate::status::{ErrorReason, StatusPublisher, WifiState};
use crate::structs::{ConnectionRequest, Network};
use anyhow::{Context, Result, anyhow};
use std::env;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::fs;
use tokio::process::{Child, Command};
use tokio::sync::RwLock;
use wpa_ctrl::{WpaController, WpaControllerBuilder};

#[derive(Debug, Clone)]
pub struct ConnectedInfo {
    pub ssid: String,
    pub ip: Option<String>,
}

pub struct WpaCtrlBackend {
    config: Arc<AppConfig>,
    status: Arc<StatusPublisher>,
    wpa_supplicant: tokio::sync::Mutex<Option<Child>>,
    hostapd: tokio::sync::Mutex<Option<Child>>,
    dnsmasq: tokio::sync::Mutex<Option<Child>>,
    cmd_ctrl: Arc<Mutex<Option<WpaController>>>,
    device_profile: RwLock<Option<DeviceProfile>>,
    scan_cache: RwLock<Vec<Network>>,
}

impl WpaCtrlBackend {
    pub fn new(config: Arc<AppConfig>, status: Arc<StatusPublisher>) -> Self {
        Self {
            config,
            status,
            wpa_supplicant: tokio::sync::Mutex::new(None),
            hostapd: tokio::sync::Mutex::new(None),
            dnsmasq: tokio::sync::Mutex::new(None),
            cmd_ctrl: Arc::new(Mutex::new(None)),
            device_profile: RwLock::new(None),
            scan_cache: RwLock::new(Vec::new()),
        }
    }

    pub fn config(&self) -> &AppConfig {
        &self.config
    }

    pub async fn shutdown(&self) {
        // 进程因错误退出或被上层要求停止时，必须清理本程序启动的系统工具。
        // 否则 Buildroot 设备上会留下孤儿 wpa_supplicant/hostapd/dnsmasq，影响下一次配网。
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

    async fn collect_device_profile(&self) -> Result<()> {
        let profile = DeviceProfile::collect(&self.config.interface.name).await;
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

    async fn refresh_scan_cache_before_ap_restore(&self, ssid: &str) {
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

    async fn ensure_station_daemon(&self) -> Result<()> {
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
                .set_error(ErrorReason::InterfaceBusy, message.clone(), None)
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
            .set_error(ErrorReason::InterfaceBusy, message.clone(), None)
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

    async fn remove_existing_wpa_socket(&self) {
        let socket_path = self.config.paths.wpa_ctrl.join(&self.config.interface.name);
        match fs::remove_file(&socket_path).await {
            Ok(()) => tracing::debug!("removed existing socket {}", socket_path.display()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => tracing::warn!("failed to remove {}: {}", socket_path.display(), err),
        }
    }

    async fn set_interface_up(&self) -> Result<()> {
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

    async fn send_cmd(&self, cmd: &str) -> Result<String> {
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

    async fn scan_once(&self) -> Result<Vec<Network>> {
        self.send_cmd("SCAN").await?;
        tokio::time::sleep(Duration::from_secs(self.config.timeouts.scan_seconds)).await;
        let output = self.send_cmd("SCAN_RESULTS").await?;
        Ok(parse_scan_results(&output))
    }

    async fn start_ap(&self) -> Result<()> {
        // AP/DHCP 仍然调用系统 hostapd 和 dnsmasq。
        // 这里不做内置 AP 或 DHCP server，实现边界保持清晰。
        tracing::info!("preparing AP services on {}", self.config.interface.name);
        let _ = self.stop_ap().await;
        self.flush_interface_ipv4().await?;
        self.apply_ap_mode_reset_quirk().await?;

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

    async fn apply_ap_mode_reset_quirk(&self) -> Result<()> {
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

        // RK + Broadcom bcmdhd 在 AP->STA 失败->AP 的快速切换中，
        // 可能保留上一次 AP beacon/security 状态，导致 hostapd 第二次启动时报
        // "Failed to set beacon parameters" 或 rsn_cap_value error。
        // 这里仅对自动识别出的 bcmdhd 设备做接口 down/up 复位，不影响其他平台。
        tracing::info!(
            "applying platform quirk rockchip_bcmdhd_ap_mode_reset on {}",
            self.config.interface.name
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

    async fn flush_interface_ipv4(&self) -> Result<()> {
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

    async fn stop_ap(&self) -> Result<()> {
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

    async fn connect_station(&self, request: &ConnectionRequest) -> Result<ConnectedInfo> {
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

    async fn run_dhcp(&self) -> Result<Option<String>> {
        tracing::info!(
            "starting DHCP client: interface={}",
            self.config.interface.name
        );
        let mut child = Command::new(&self.config.commands.udhcpc)
            .arg("-i")
            .arg(&self.config.interface.name)
            .arg("-q")
            .arg("-n")
            .spawn()
            .context("failed to start udhcpc")?;

        let status = match tokio::time::timeout(
            Duration::from_secs(self.config.timeouts.dhcp_seconds),
            child.wait(),
        )
        .await
        {
            Ok(result) => result.context("failed to wait for udhcpc")?,
            Err(_) => {
                let _ = child.kill().await;
                tracing::warn!(
                    "DHCP timed out: interface={} timeout={}s",
                    self.config.interface.name,
                    self.config.timeouts.dhcp_seconds
                );
                return Err(anyhow!("dhcp_timeout"));
            }
        };

        if !status.success() {
            tracing::warn!(
                "DHCP failed: interface={} status={}",
                self.config.interface.name,
                status
            );
            return Err(anyhow!("dhcp_failed"));
        }

        let ip = self.read_interface_ipv4().await?;
        tracing::info!(
            "DHCP completed: interface={} ip={:?}",
            self.config.interface.name,
            ip
        );
        Ok(ip)
    }

    async fn read_interface_ipv4(&self) -> Result<Option<String>> {
        // DHCP 客户端成功后，再用系统 ip 命令读取接口地址。
        // udhcpc 的脚本行为在不同 Buildroot 产品上可能不同，直接解析 stdout 不稳。
        let output = Command::new(&self.config.commands.ip)
            .arg("-4")
            .arg("-o")
            .arg("addr")
            .arg("show")
            .arg("dev")
            .arg(&self.config.interface.name)
            .output()
            .await
            .context("failed to read interface IPv4 address")?;

        if !output.status.success() {
            tracing::warn!(
                "failed to read IPv4 address: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            return Ok(None);
        }

        Ok(parse_ipv4_addr(&String::from_utf8_lossy(&output.stdout)))
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

fn parse_wpa_state(status: &str) -> Option<&str> {
    wpa_status_field(status, "wpa_state")
}

fn wpa_status_field<'a>(status: &'a str, name: &str) -> Option<&'a str> {
    status.lines().find_map(|line| {
        let (key, value) = line.split_once('=')?;
        (key == name).then_some(value)
    })
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

fn parse_ipv4_addr(output: &str) -> Option<String> {
    let mut parts = output.split_whitespace();
    while let Some(part) = parts.next() {
        if part != "inet" {
            continue;
        }

        let cidr = parts.next()?;
        return Some(
            cidr.split_once('/')
                .map_or(cidr, |(address, _)| address)
                .to_string(),
        );
    }

    None
}

fn format_dnsmasq_dhcp_range(iface: &str, range: &str) -> String {
    if range.starts_with("interface:") {
        range.to_string()
    } else {
        format!("interface:{},{}", iface, range)
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

    #[test]
    fn parse_ipv4_addr_extracts_cidr_address() {
        let output = "2: wlan0    inet 192.168.1.24/24 brd 192.168.1.255 scope global wlan0\n";

        assert_eq!(parse_ipv4_addr(output).as_deref(), Some("192.168.1.24"));
    }

    #[test]
    fn format_wpa_psk_quotes_passphrases_but_not_raw_psk() {
        let raw = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

        assert_eq!(format_wpa_psk(raw), raw);
        assert_eq!(format_wpa_psk("plain\"pass"), "\"plain\\\"pass\"");
    }

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
