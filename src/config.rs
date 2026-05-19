use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};
use std::env;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::str::FromStr;

const DEFAULT_CONFIG_PATH: &str = "/etc/wlan0-bootstrap/config.toml";
const BUILTIN_CONFIG_TOML: &str = include_str!("../configs.toml");

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AppConfig {
    pub interface: InterfaceConfig,
    pub paths: PathsConfig,
    pub ap: ApConfig,
    pub timeouts: TimeoutConfig,
    pub commands: CommandConfig,
    pub ownership: OwnershipConfig,
    #[serde(default)]
    pub platform: PlatformConfig,
    #[serde(default)]
    pub discovery: DiscoveryConfig,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct InterfaceConfig {
    pub name: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PathsConfig {
    pub data_dir: PathBuf,
    pub run_dir: PathBuf,
    pub wpa_config: PathBuf,
    pub wpa_ctrl: PathBuf,
    pub hostapd_config: PathBuf,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ApConfig {
    pub ssid_prefix: String,
    pub password: String,
    pub gateway_cidr: String,
    pub bind_addr: String,
    pub dhcp_range: String,
    pub hw_mode: String,
    pub channel: u8,
    pub wpa: u8,
    pub wpa_key_mgmt: String,
    pub wpa_pairwise: String,
    pub rsn_pairwise: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TimeoutConfig {
    pub scan_seconds: u64,
    pub connect_seconds: u64,
    pub dhcp_seconds: u64,
    pub provisioning_idle_seconds: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CommandConfig {
    pub wpa_supplicant: String,
    pub hostapd: String,
    pub dnsmasq: String,
    pub ip: String,
    pub udhcpc: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct OwnershipConfig {
    pub force_takeover: bool,
    pub wpa_group: String,
    pub wpa_update_config: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PlatformConfig {
    pub auto_driver_quirks: bool,
    pub ap_mode_reset_delay_ms: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DiscoveryConfig {
    pub mdns_enabled: bool,
    pub hostname_prefix: String,
    pub hostname: String,
    pub http_service_enabled: bool,
    pub http_service_type: String,
    pub http_service_name: String,
}

impl Default for PlatformConfig {
    fn default() -> Self {
        Self {
            auto_driver_quirks: true,
            ap_mode_reset_delay_ms: 600,
        }
    }
}

impl Default for DiscoveryConfig {
    fn default() -> Self {
        Self {
            mdns_enabled: true,
            hostname_prefix: "wlan-bootstrap".to_string(),
            hostname: String::new(),
            http_service_enabled: true,
            http_service_type: "_http._tcp.local.".to_string(),
            http_service_name: "wlan bootstrap".to_string(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct CliOptions {
    pub config_path: Option<PathBuf>,
}

impl CliOptions {
    pub fn parse() -> Result<Self> {
        let mut args = env::args_os().skip(1);
        let mut config_path = None;

        while let Some(arg) = args.next() {
            if arg == "--config" {
                let value = args
                    .next()
                    .ok_or_else(|| anyhow!("--config requires a path"))?;
                config_path = Some(PathBuf::from(value));
                continue;
            }

            if arg == "--help" || arg == "-h" {
                println!("usage: wlan0-bootstrap [--config /path/to/config.toml]");
                std::process::exit(0);
            }

            return Err(anyhow!("unknown argument: {:?}", arg));
        }

        Ok(Self { config_path })
    }
}

impl AppConfig {
    pub fn load(options: &CliOptions) -> Result<Self> {
        // 产品部署优先读取 /etc 下的运行时配置。
        // 仓库内 configs.toml 只作为开发和缺省兜底，不应成为量产设备的唯一配置来源。
        let config = match &options.config_path {
            Some(path) => Self::load_from_path(path)?,
            None => match Self::load_from_path(Path::new(DEFAULT_CONFIG_PATH)) {
                Ok(config) => config,
                Err(err) if is_not_found(&err) => {
                    tracing::warn!(
                        "Config file {} not found; using built-in fallback config",
                        DEFAULT_CONFIG_PATH
                    );
                    Self::load_from_str(BUILTIN_CONFIG_TOML)
                        .context("failed to parse built-in config")?
                }
                Err(err) => return Err(err),
            },
        };

        config.validate()?;
        Ok(config)
    }

    pub fn load_from_path(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read config {}", path.display()))?;
        Self::load_from_str(&content)
            .with_context(|| format!("failed to parse config {}", path.display()))
    }

    pub fn load_from_str(content: &str) -> Result<Self> {
        toml::from_str(content).context("invalid TOML config")
    }

    pub fn bind_addr(&self) -> Result<SocketAddr> {
        SocketAddr::from_str(&self.ap.bind_addr)
            .with_context(|| format!("invalid AP bind address {}", self.ap.bind_addr))
    }

    pub fn networks_path(&self) -> PathBuf {
        // 已知网络属于持久化数据，放在 data_dir 下，适配只读 rootfs 的 Buildroot 设备。
        self.paths.data_dir.join("networks.toml")
    }

    pub fn status_path(&self) -> PathBuf {
        self.paths.run_dir.join("status.json")
    }

    pub fn event_socket_path(&self) -> PathBuf {
        self.paths.run_dir.join("events.sock")
    }

    pub fn ap_ssid(&self) -> String {
        self.ap.ssid_prefix.clone()
    }

    fn validate(&self) -> Result<()> {
        // 配置校验只检查会导致启动失败或死循环的硬约束。
        // 具体命令是否存在、接口是否存在由 preflight 在运行环境中检查。
        if self.interface.name.trim().is_empty() {
            return Err(anyhow!("interface.name must not be empty"));
        }
        if self.ap.password.len() < 8 {
            return Err(anyhow!("ap.password must contain at least 8 characters"));
        }
        if self.timeouts.scan_seconds == 0 {
            return Err(anyhow!("timeouts.scan_seconds must be greater than zero"));
        }
        if self.timeouts.connect_seconds == 0 {
            return Err(anyhow!(
                "timeouts.connect_seconds must be greater than zero"
            ));
        }
        if self.timeouts.dhcp_seconds == 0 {
            return Err(anyhow!("timeouts.dhcp_seconds must be greater than zero"));
        }
        if self.timeouts.provisioning_idle_seconds == 0 {
            return Err(anyhow!(
                "timeouts.provisioning_idle_seconds must be greater than zero"
            ));
        }
        if self.discovery.mdns_enabled && self.discovery.hostname_prefix.trim().is_empty() {
            return Err(anyhow!("discovery.hostname_prefix must not be empty"));
        }
        if self.discovery.http_service_enabled
            && !self.discovery.http_service_type.ends_with(".local.")
        {
            return Err(anyhow!(
                "discovery.http_service_type must be a fully qualified .local. service type"
            ));
        }
        self.bind_addr()?;
        Ok(())
    }
}

fn is_not_found(err: &anyhow::Error) -> bool {
    err.chain().any(|source| {
        source
            .downcast_ref::<std::io::Error>()
            .is_some_and(|io_err| io_err.kind() == std::io::ErrorKind::NotFound)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_config() -> AppConfig {
        AppConfig::load_from_str(
            r#"
[interface]
name = "wlan0"

[paths]
data_dir = "/data/wlan0-bootstrap"
run_dir = "/run/wlan0-bootstrap"
wpa_config = "/run/wlan0-bootstrap/wpa_supplicant.conf"
wpa_ctrl = "/run/wpa_supplicant"
hostapd_config = "/run/wlan0-bootstrap/hostapd.conf"

[ap]
ssid_prefix = "wlan0-bootstrap"
password = "change-me"
gateway_cidr = "192.168.4.1/24"
bind_addr = "192.168.4.1:80"
dhcp_range = "192.168.4.100,192.168.4.200,12h"
hw_mode = "g"
channel = 6
wpa = 2
wpa_key_mgmt = "WPA-PSK"
wpa_pairwise = "CCMP"
rsn_pairwise = "CCMP"

[timeouts]
scan_seconds = 10
connect_seconds = 30
dhcp_seconds = 20
provisioning_idle_seconds = 600

[commands]
wpa_supplicant = "wpa_supplicant"
hostapd = "hostapd"
dnsmasq = "dnsmasq"
ip = "ip"
udhcpc = "udhcpc"

[ownership]
force_takeover = false
wpa_group = "netdev"
wpa_update_config = false

[discovery]
mdns_enabled = true
hostname_prefix = "wlan-bootstrap"
hostname = ""
http_service_enabled = true
http_service_type = "_http._tcp.local."
http_service_name = "wlan bootstrap"
"#,
        )
        .expect("test config should parse")
    }

    #[test]
    fn validate_rejects_zero_runtime_timeouts() {
        let mut config = valid_config();
        config.timeouts.dhcp_seconds = 0;
        assert!(config.validate().is_err());

        let mut config = valid_config();
        config.timeouts.provisioning_idle_seconds = 0;
        assert!(config.validate().is_err());
    }

    #[test]
    fn derived_paths_use_configured_directories() {
        let config = valid_config();

        assert_eq!(
            config.networks_path(),
            PathBuf::from("/data/wlan0-bootstrap/networks.toml")
        );
        assert_eq!(
            config.status_path(),
            PathBuf::from("/run/wlan0-bootstrap/status.json")
        );
        assert_eq!(
            config.event_socket_path(),
            PathBuf::from("/run/wlan0-bootstrap/events.sock")
        );
    }

    #[test]
    fn discovery_defaults_are_enabled() {
        let config = AppConfig::load_from_str(
            r#"
[interface]
name = "wlan0"

[paths]
data_dir = "/data/wlan0-bootstrap"
run_dir = "/run/wlan0-bootstrap"
wpa_config = "/run/wlan0-bootstrap/wpa_supplicant.conf"
wpa_ctrl = "/run/wpa_supplicant"
hostapd_config = "/run/wlan0-bootstrap/hostapd.conf"

[ap]
ssid_prefix = "wlan0-bootstrap"
password = "change-me"
gateway_cidr = "192.168.4.1/24"
bind_addr = "192.168.4.1:80"
dhcp_range = "192.168.4.100,192.168.4.200,12h"
hw_mode = "g"
channel = 6
wpa = 2
wpa_key_mgmt = "WPA-PSK"
wpa_pairwise = "CCMP"
rsn_pairwise = "CCMP"

[timeouts]
scan_seconds = 10
connect_seconds = 30
dhcp_seconds = 20
provisioning_idle_seconds = 600

[commands]
wpa_supplicant = "wpa_supplicant"
hostapd = "hostapd"
dnsmasq = "dnsmasq"
ip = "ip"
udhcpc = "udhcpc"

[ownership]
force_takeover = false
wpa_group = "netdev"
wpa_update_config = false
"#,
        )
        .expect("config without discovery should parse");

        assert!(config.discovery.mdns_enabled);
        assert_eq!(config.discovery.hostname_prefix, "wlan-bootstrap");
        assert_eq!(config.discovery.http_service_type, "_http._tcp.local.");
    }
}
