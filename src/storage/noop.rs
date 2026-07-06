use async_trait::async_trait;

use crate::{
    domain::{BboTick, InstrumentCatalog},
    storage::BboSink,
};

#[derive(Debug, Default)]
pub struct NoopSink;

#[async_trait]
impl BboSink for NoopSink {
    async fn write_catalog(&self, _catalog: &InstrumentCatalog) -> anyhow::Result<()> {
        Ok(())
    }

    async fn write_tick(&self, _tick: &BboTick) -> anyhow::Result<()> {
        Ok(())
    }

    async fn flush(&self) -> anyhow::Result<()> {
        Ok(())
    }
}
