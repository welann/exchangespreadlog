use std::sync::Arc;

use anyhow::anyhow;
use async_trait::async_trait;

use crate::{domain::BboTick, storage::BboSink};

pub struct MultiSink {
    sinks: Vec<Arc<dyn BboSink>>,
}

impl MultiSink {
    pub fn new(sinks: Vec<Arc<dyn BboSink>>) -> Self {
        Self { sinks }
    }

    fn combine_errors(action: &str, errors: Vec<anyhow::Error>) -> anyhow::Error {
        let details = errors
            .into_iter()
            .map(|err| err.to_string())
            .collect::<Vec<_>>()
            .join("; ");
        anyhow!("{action} failed for one or more sinks: {details}")
    }
}

#[async_trait]
impl BboSink for MultiSink {
    async fn write(&self, tick: &BboTick) -> anyhow::Result<()> {
        let mut errors = Vec::new();
        for sink in &self.sinks {
            if let Err(err) = sink.write(tick).await {
                errors.push(err);
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(Self::combine_errors("write", errors))
        }
    }

    async fn flush(&self) -> anyhow::Result<()> {
        let mut errors = Vec::new();
        for sink in &self.sinks {
            if let Err(err) = sink.flush().await {
                errors.push(err);
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(Self::combine_errors("flush", errors))
        }
    }
}
