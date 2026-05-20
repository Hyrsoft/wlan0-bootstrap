use crate::config::{AppConfig, DiscoveryConfig};
use anyhow::{Context, Result, anyhow};
use mdns_sd::{ServiceDaemon, ServiceInfo};
use std::io::Read;
use std::net::{IpAddr, Ipv4Addr};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::fs;

const DEVICE_ID_FILE: &str = "device-id";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveryInfo {
    pub hostname: String,
    pub address: String,
    pub http_port: u16,
}

impl DiscoveryInfo {
    fn mdns_hostname(&self) -> String {
        format!("{}.", self.hostname.trim_end_matches('.'))
    }
}

pub struct MdnsPublisher {
    config: DiscoveryConfig,
    daemon: Option<ServiceDaemon>,
    registered_fullname: Option<String>,
    published_hostname: Option<String>,
}

impl MdnsPublisher {
    pub fn new(config: DiscoveryConfig) -> Self {
        Self {
            config,
            daemon: None,
            registered_fullname: None,
            published_hostname: None,
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.config.mdns_enabled
    }

    pub fn published_hostname(&self) -> Option<String> {
        self.published_hostname.clone()
    }

    pub async fn publish(&mut self, info: &DiscoveryInfo) -> Result<()> {
        if !self.config.mdns_enabled {
            return Ok(());
        }

        self.stop().await;
        let http_service_enabled = self.config.http_service_enabled;
        let http_service_type = self.config.http_service_type.clone();
        let http_service_name = normalize_service_name(&self.config.http_service_name);
        let daemon = self.ensure_daemon()?;

        if http_service_enabled {
            let address = info
                .address
                .parse::<Ipv4Addr>()
                .with_context(|| format!("invalid discovery IPv4 address {}", info.address))?;
            let properties = [
                ("path", "/"),
                ("state", "connected"),
                ("hostname", info.hostname.as_str()),
            ];
            let service = ServiceInfo::new(
                &http_service_type,
                &http_service_name,
                &info.mdns_hostname(),
                IpAddr::V4(address),
                info.http_port,
                &properties[..],
            )
            .context("failed to create mDNS service info")?;
            let fullname = service.get_fullname().to_string();
            daemon
                .register(service)
                .context("failed to register mDNS HTTP service")?;
            self.registered_fullname = Some(fullname);
        }

        self.published_hostname = Some(info.hostname.clone());
        Ok(())
    }

    pub async fn stop(&mut self) {
        if let (Some(daemon), Some(fullname)) = (&self.daemon, self.registered_fullname.take())
            && let Err(err) = daemon.unregister(&fullname)
        {
            tracing::warn!("failed to unregister mDNS service {}: {}", fullname, err);
        }
        self.published_hostname = None;
    }

    pub async fn shutdown(&mut self) {
        self.stop().await;
        if let Some(daemon) = self.daemon.take() {
            let _ = daemon.shutdown();
        }
    }

    fn ensure_daemon(&mut self) -> Result<&ServiceDaemon> {
        if self.daemon.is_none() {
            self.daemon = Some(ServiceDaemon::new().context("failed to start mDNS daemon")?);
        }
        Ok(self.daemon.as_ref().expect("daemon was just initialized"))
    }
}

pub async fn build_discovery_info(config: &AppConfig, address: &str) -> Result<DiscoveryInfo> {
    let hostname = resolve_discovery_hostname(config).await?;
    let http_port = config.bind_addr()?.port();
    Ok(DiscoveryInfo {
        hostname,
        address: address.to_string(),
        http_port,
    })
}

pub async fn resolve_discovery_hostname(config: &AppConfig) -> Result<String> {
    if !config.discovery.hostname.trim().is_empty() {
        return Ok(format_local_hostname(&config.discovery.hostname));
    }

    let id_path = config.paths.data_dir.join(DEVICE_ID_FILE);
    let suffix = load_or_create_device_id(&id_path).await?;
    Ok(format!(
        "{}-{}.local",
        sanitize_label(&config.discovery.hostname_prefix),
        suffix
    ))
}

async fn load_or_create_device_id(path: &Path) -> Result<String> {
    match fs::read_to_string(path).await {
        Ok(value) => {
            if let Some(sanitized) = sanitize_device_id(&value) {
                return Ok(sanitized);
            }
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => return Err(err).with_context(|| format!("failed to read {}", path.display())),
    }

    let generated = generate_device_id();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .await
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    fs::write(path, generated.as_bytes())
        .await
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(generated)
}

fn generate_device_id() -> String {
    let mut random = [0_u8; 3];
    if std::fs::File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut random))
        .is_ok()
    {
        return format!("{:02x}{:02x}{:02x}", random[0], random[1], random[2]);
    }

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let mixed = now ^ ((std::process::id() as u128) << 32);
    format!("{:06x}", mixed & 0x00ff_ffff)
}

fn format_local_hostname(value: &str) -> String {
    let label = value
        .trim()
        .trim_end_matches('.')
        .trim_end_matches(".local");
    format!("{}.local", sanitize_label(label))
}

fn sanitize_label(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut last_dash = false;
    for ch in value.chars().flat_map(char::to_lowercase) {
        let next = if ch.is_ascii_lowercase() || ch.is_ascii_digit() {
            Some(ch)
        } else if ch == '-' || ch == '_' || ch.is_whitespace() || ch == '.' {
            Some('-')
        } else {
            None
        };

        let Some(next) = next else {
            continue;
        };
        if next == '-' {
            if out.is_empty() || last_dash {
                continue;
            }
            last_dash = true;
        } else {
            last_dash = false;
        }
        out.push(next);
        if out.len() == 63 {
            break;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        "wlan-bootstrap".to_string()
    } else {
        out
    }
}

fn sanitize_device_id(value: &str) -> Option<String> {
    let id = value
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .take(6)
        .collect::<String>();
    (id.len() == 6).then_some(id)
}

fn normalize_service_name(value: &str) -> String {
    let trimmed = value.trim();
    let name = if trimmed.is_empty() {
        "wlan bootstrap"
    } else {
        trimmed
    };
    if name.len() <= 30 {
        name.to_string()
    } else {
        name.chars().take(30).collect()
    }
}

pub fn validate_discovery_address(address: &str) -> Result<()> {
    address
        .parse::<Ipv4Addr>()
        .map(|_| ())
        .map_err(|_| anyhow!("discovery requires an IPv4 address, got {}", address))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_hostname_is_sanitized_and_keeps_local_suffix() {
        assert_eq!(
            format_local_hostname("My Device_01.local."),
            "my-device-01.local"
        );
        assert_eq!(format_local_hostname("bad!!!"), "bad.local");
    }

    #[test]
    fn label_sanitization_collapses_invalid_characters() {
        assert_eq!(sanitize_label("WLAN Bootstrap__A1"), "wlan-bootstrap-a1");
        assert_eq!(sanitize_label("---"), "wlan-bootstrap");
        assert!(sanitize_label(&"a".repeat(80)).len() <= 63);
    }

    #[test]
    fn device_id_is_exactly_six_lowercase_alnum_chars() {
        assert_eq!(
            sanitize_device_id(" AB:CD:12:34 "),
            Some("abcd12".to_string())
        );
        assert_eq!(sanitize_device_id("abc"), None);

        let generated = generate_device_id();
        assert_eq!(generated.len(), 6);
        assert!(generated.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert!(generated.bytes().all(|byte| !byte.is_ascii_uppercase()));
    }

    #[test]
    fn discovery_info_formats_http_url() {
        let info = DiscoveryInfo {
            hostname: "wlan-bootstrap-123456.local".to_string(),
            address: "192.168.1.88".to_string(),
            http_port: 80,
        };

        assert_eq!(info.mdns_hostname(), "wlan-bootstrap-123456.local.");
    }

    #[test]
    fn validates_ipv4_discovery_address() {
        assert!(validate_discovery_address("192.168.1.88").is_ok());
        assert!(validate_discovery_address("not-an-ip").is_err());
    }
}
