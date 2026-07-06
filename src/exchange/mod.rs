pub mod hyperliquid;
pub mod lighter;
pub mod risex;
pub mod zero_one;

use std::{collections::HashMap, future::Future};

use async_trait::async_trait;
use tokio::{
    sync::{mpsc::Sender, watch},
    time,
};
use tracing::warn;

use crate::{
    domain::{BboTick, InstrumentCatalog, InstrumentRef, MarketEvent},
    ingest::supervisor::Backoff,
};

#[async_trait]
pub trait ExchangeAdapter: Send + Sync {
    async fn run(
        &self,
        tx: Sender<MarketEvent>,
        shutdown: watch::Receiver<bool>,
    ) -> anyhow::Result<()>;
}

pub async fn run_with_reconnect<F, Fut>(
    venue: &'static str,
    tx: Sender<MarketEvent>,
    shutdown: watch::Receiver<bool>,
    mut run_once: F,
) -> anyhow::Result<()>
where
    F: FnMut(Sender<MarketEvent>, watch::Receiver<bool>) -> Fut + Send,
    Fut: Future<Output = anyhow::Result<()>> + Send,
{
    let mut backoff = Backoff::default();
    while !*shutdown.borrow() {
        match run_once(tx.clone(), shutdown.clone()).await {
            Ok(()) => return Ok(()),
            Err(err) => {
                let sleep = backoff.next_delay();
                warn!(venue = %venue, error = %err, ?sleep, "adapter restarting");
                time::sleep(sleep).await;
            }
        }
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub struct CatalogIndex {
    instruments: Vec<InstrumentCatalog>,
    refs_by_feed_key: HashMap<String, InstrumentRef>,
}

impl CatalogIndex {
    pub fn new(instruments: Vec<InstrumentCatalog>) -> Self {
        let mut refs_by_feed_key = HashMap::new();
        for instrument in &instruments {
            let instrument_ref = instrument.instrument_ref();
            refs_by_feed_key.insert(instrument.instrument_id.clone(), instrument_ref.clone());
            refs_by_feed_key.insert(instrument.raw_symbol.clone(), instrument_ref.clone());
            if let Some(feed_symbol) = &instrument.feed_symbol {
                refs_by_feed_key.insert(feed_symbol.clone(), instrument_ref);
            }
        }

        Self {
            instruments,
            refs_by_feed_key,
        }
    }

    pub fn instruments(&self) -> &[InstrumentCatalog] {
        &self.instruments
    }

    pub fn resolve(&self, feed_key: &str) -> Option<InstrumentRef> {
        self.refs_by_feed_key.get(feed_key).cloned()
    }

    pub fn retarget_tick(&self, mut tick: BboTick) -> Option<BboTick> {
        let instrument = self.resolve(&tick.instrument.instrument_id)?;
        tick.instrument = instrument;
        Some(tick)
    }
}
