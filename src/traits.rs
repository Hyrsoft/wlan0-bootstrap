use anyhow::Result;
use async_trait::async_trait;
use std::borrow::Cow;

/// 一个提供 UI 静态资产的通用 Trait。
///
/// `Send + Sync` 约束是必须的，因为它将被用于 Axum 的共享状态 (State) 中。
#[async_trait]
pub trait UiAssetProvider: Send + Sync {
    /// 根据路径获取一个资产。
    ///
    /// 返回元组 (数据, Mime类型字符串)
    async fn get_asset(&self, path: &str) -> Result<(Cow<'static, [u8]>, String)>;
}
