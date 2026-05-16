use crate::config::AppConfig;
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
        }
    }

    pub fn config(&self) -> &AppConfig {
        &self.config
    }

    pub async fn prepare(&self) -> Result<()> {
        self.status
            .set_state(WifiState::Preflight, None, None)
            .await?;
        self.preflight().await?;
        self.ensure_station_daemon().await?;
        Ok(())
    }

    pub async fn scan(&self) -> Result<Vec<Network>> {
        self.status
            .set_state(WifiState::Scanning, None, None)
            .await?;
        let mut networks = Vec::new();
        for attempt in 1..=3 {
            tracing::info!("Scanning Wi-Fi networks, attempt {}", attempt);
            networks = self.scan_once().await?;
            if !networks.is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_secs(self.config.timeouts.scan_seconds)).await;
        }

        if networks.is_empty() {
            self.status
                .set_error(ErrorReason::ScanFailed, "scan returned no networks", None)
                .await?;
        }

        Ok(networks)
    }

    pub async fn start_provisioning_ap(&self) -> Result<()> {
        self.status
            .set_state(WifiState::ProvisioningApStarting, None, None)
            .await?;
        self.start_ap().await?;
        self.status
            .set_state(
                WifiState::ProvisioningApRunning,
                Some(self.config.ap_ssid()),
                Some(self.config.ap.bind_addr.clone()),
            )
            .await?;
        Ok(())
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
        self.status
            .set_state(
                WifiState::ProvisioningConnecting,
                Some(request.ssid.clone()),
                None,
            )
            .await?;
        let _ = self.stop_ap().await;

        match self.connect_station(request).await {
            Ok(info) => Ok(info),
            Err(err) => {
                let message = err.to_string();
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

    pub async fn monitor_until_disconnected(&self, ssid: &str) {
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
        self.write_wpa_config().await?;
        self.remove_stale_wpa_socket().await;
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

    async fn write_wpa_config(&self) -> Result<()> {
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

    async fn remove_stale_wpa_socket(&self) {
        let socket_path = self.config.paths.wpa_ctrl.join(&self.config.interface.name);
        match fs::remove_file(&socket_path).await {
            Ok(()) => tracing::debug!("removed stale socket {}", socket_path.display()),
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

    async fn send_cmd(&self, cmd: &str) -> Result<String> {
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
        let _ = self.stop_ap().await;

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
            .arg("-B")
            .spawn()
            .context("failed to start hostapd")?;
        *self.hostapd.lock().await = Some(hostapd);

        let ap_ip = self
            .config
            .ap
            .gateway_cidr
            .split_once('/')
            .map(|(ip, _)| ip)
            .unwrap_or(self.config.ap.gateway_cidr.as_str());
        let dnsmasq = Command::new(&self.config.commands.dnsmasq)
            .arg(format!("--interface={}", self.config.interface.name))
            .arg(format!("--dhcp-range={}", self.config.ap.dhcp_range))
            .arg(format!("--address=/#/{}", ap_ip))
            .arg("--no-resolv")
            .arg("--no-hosts")
            .arg("--no-daemon")
            .spawn()
            .context("failed to start dnsmasq")?;
        *self.dnsmasq.lock().await = Some(dnsmasq);

        Ok(())
    }

    async fn stop_ap(&self) -> Result<()> {
        if let Some(mut child) = self.dnsmasq.lock().await.take() {
            let _ = child.kill().await;
        }
        if let Some(mut child) = self.hostapd.lock().await.take() {
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
        let net_id = self.add_network().await?;
        let result = self.configure_and_enable_network(net_id, request).await;
        if let Err(err) = result {
            let _ = self.send_cmd(&format!("REMOVE_NETWORK {}", net_id)).await;
            return Err(err);
        }

        let connected = self.wait_for_connection(request, net_id).await?;
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
            self.send_cmd(&format!(
                "SET_NETWORK {} psk {}",
                net_id,
                quote_wpa_string(&request.password)
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

        loop {
            if started.elapsed() > timeout {
                let _ = self.send_cmd(&format!("REMOVE_NETWORK {}", net_id)).await;
                return Err(anyhow!("association_timeout"));
            }

            tokio::time::sleep(Duration::from_secs(2)).await;
            let status = self.send_cmd("STATUS").await?;
            match parse_wpa_state(&status) {
                Some("COMPLETED") => {
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
                    let _ = self.send_cmd(&format!("REMOVE_NETWORK {}", net_id)).await;
                    return Err(anyhow!("network_not_found_or_wrong_password"));
                }
                _ => {}
            }
        }
    }

    async fn run_dhcp(&self) -> Result<Option<String>> {
        let status = Command::new(&self.config.commands.udhcpc)
            .arg("-i")
            .arg(&self.config.interface.name)
            .arg("-q")
            .arg("-n")
            .status()
            .await
            .context("failed to run udhcpc")?;

        if !status.success() {
            return Err(anyhow!("dhcp_failed"));
        }

        Ok(None)
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
    status.lines().find_map(|line| {
        let (key, value) = line.split_once('=')?;
        (key == "wpa_state").then_some(value)
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

fn classify_connection_error(message: &str) -> ErrorReason {
    if message.contains("dhcp_failed") {
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
