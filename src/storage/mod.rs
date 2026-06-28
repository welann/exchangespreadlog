pub mod jsonl;
pub mod noop;

use async_trait::async_trait;

use crate::domain::BboTick;

#[async_trait]
pub trait BboSink: Send + Sync {
    async fn write(&self, tick: &BboTick) -> anyhow::Result<()>;
    async fn flush(&self) -> anyhow::Result<()>;
}
