use crate::traits::UiAssetProvider;
use anyhow::{Result, anyhow};
use async_trait::async_trait;
use rust_embed::RustEmbed;
use std::borrow::Cow;

// 用于在二进制文件中嵌入其他资源，例如Web UI

#[derive(RustEmbed)]
#[folder = "ui/"]
struct Asset;

/// A UI asset provider that serves files embedded into the binary.
#[derive(Debug, Default)]
pub struct EmbedFrontend;

impl EmbedFrontend {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl UiAssetProvider for EmbedFrontend {
    async fn get_asset(&self, path: &str) -> Result<(Cow<'static, [u8]>, String)> {
        // The `Asset` struct used here is determined at compile time by the features above.
        let asset = Asset::get(path).ok_or_else(|| {
            // 提供更清晰的错误日志
            tracing::debug!("Asset not found: {}", path);
            anyhow!("AssetNotFound: {}", path)
        })?;

        let mime = mime_guess::from_path(path)
            .first_or_octet_stream()
            .to_string();

        Ok((asset.data, mime))
    }
}
