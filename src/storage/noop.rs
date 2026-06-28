use async_trait::async_trait;

use crate::{domain::BboTick, storage::BboSink};

#[derive(Debug, Default)]
pub struct NoopSink;

#[async_trait]
impl BboSink for NoopSink {
    async fn write(&self, _tick: &BboTick) -> anyhow::Result<()> {
        Ok(())
    }

    async fn flush(&self) -> anyhow::Result<()> {
        Ok(())
    }
}
