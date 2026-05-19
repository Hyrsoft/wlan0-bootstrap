use super::WpaCtrlBackend;
use anyhow::{Context, Result, anyhow};
use std::time::Duration;
use tokio::process::Command;

impl WpaCtrlBackend {
    pub(super) async fn run_dhcp(&self) -> Result<Option<String>> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_ipv4_addr_extracts_cidr_address() {
        let output = "2: wlan0    inet 192.168.1.24/24 brd 192.168.1.255 scope global wlan0\n";

        assert_eq!(parse_ipv4_addr(output).as_deref(), Some("192.168.1.24"));
    }
}
