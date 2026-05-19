use super::{ConnectedInfo, WpaCtrlBackend};
use crate::discovery::{DiscoveryInfo, build_discovery_info, validate_discovery_address};
use anyhow::Result;

impl WpaCtrlBackend {
    pub async fn publish_connected_discovery(
        &self,
        connected: &ConnectedInfo,
    ) -> Result<Option<DiscoveryInfo>> {
        let mut mdns = self.mdns.lock().await;
        if !mdns.is_enabled() {
            self.status.set_mdns_disabled().await?;
            return Ok(None);
        }
        if !self.config().discovery.http_service_enabled {
            self.status.set_mdns_disabled().await?;
            return Ok(None);
        }

        let Some(address) = connected.ip.as_deref() else {
            self.status
                .set_mdns_failed(None, "connected interface has no IPv4 address")
                .await?;
            return Ok(None);
        };

        if let Err(err) = validate_discovery_address(address) {
            self.status.set_mdns_failed(None, err.to_string()).await?;
            return Ok(None);
        }

        let info = match build_discovery_info(self.config(), address).await {
            Ok(info) => info,
            Err(err) => {
                let hostname = self.status.snapshot().await.hostname;
                self.status
                    .set_mdns_failed(hostname, err.to_string())
                    .await?;
                return Ok(None);
            }
        };

        self.status
            .set_mdns_publishing(info.hostname.clone(), info.address.clone(), info.http_port)
            .await?;

        match mdns.publish(&info).await {
            Ok(()) => {
                self.status
                    .set_mdns_published(info.hostname.clone(), info.address.clone(), info.http_port)
                    .await?;
                Ok(Some(info))
            }
            Err(err) => {
                self.status
                    .set_mdns_failed(Some(info.hostname.clone()), err.to_string())
                    .await?;
                Ok(Some(info))
            }
        }
    }

    pub async fn stop_discovery(&self) {
        let mut mdns = self.mdns.lock().await;
        let hostname = mdns.published_hostname();
        mdns.stop().await;
        if hostname.is_some() {
            let _ = self.status.set_mdns_stopped(hostname).await;
        }
    }
}
