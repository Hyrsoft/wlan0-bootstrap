mod ap;
mod dhcp;
mod lifecycle;
mod provisioning;
mod scan;
mod station;
mod wpa;

use crate::config::AppConfig;
use crate::device_profile::DeviceProfile;
use crate::status::StatusPublisher;
use crate::structs::Network;
use std::sync::{Arc, Mutex};
use tokio::process::Child;
use tokio::sync::RwLock;
use wpa_ctrl::WpaController;

#[derive(Debug, Clone)]
pub struct ConnectedInfo {
    pub ssid: String,
    pub ip: Option<String>,
}

pub struct WpaCtrlBackend {
    pub(super) config: Arc<AppConfig>,
    pub(super) status: Arc<StatusPublisher>,
    pub(super) wpa_supplicant: tokio::sync::Mutex<Option<Child>>,
    pub(super) hostapd: tokio::sync::Mutex<Option<Child>>,
    pub(super) dnsmasq: tokio::sync::Mutex<Option<Child>>,
    pub(super) cmd_ctrl: Arc<Mutex<Option<WpaController>>>,
    pub(super) device_profile: RwLock<Option<DeviceProfile>>,
    pub(super) scan_cache: RwLock<Vec<Network>>,
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
}
