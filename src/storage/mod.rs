pub mod clickhouse;
pub mod jsonl;
pub mod multi;
pub mod noop;

use async_trait::async_trait;

use crate::domain::{BboTick, InstrumentCatalog};

#[async_trait]
pub trait BboSink: Send + Sync {
    async fn write_catalog(&self, catalog: &InstrumentCatalog) -> anyhow::Result<()>;
    async fn write_tick(&self, tick: &BboTick) -> anyhow::Result<()>;
    async fn flush(&self) -> anyhow::Result<()>;
}
