pub mod dedupe;
pub mod normalizer;

use std::sync::Arc;

use anyhow::Context;
use tokio::{sync::mpsc::Receiver, task};
use tracing::debug;

use crate::{domain::MarketEvent, store::LiveBookStore, wal::DurableEventLog};

const WAL_BATCH_SIZE: usize = 512;

pub async fn run(
    mut receiver: Receiver<MarketEvent>,
    wal: DurableEventLog,
    store: LiveBookStore,
    stale_after_ms: i64,
) -> anyhow::Result<()> {
    let mut dedupe = dedupe::DedupeFilter::default();
    while let Some(first) = receiver.recv().await {
        let mut incoming = Vec::with_capacity(WAL_BATCH_SIZE);
        incoming.push(first);
        while incoming.len() < WAL_BATCH_SIZE {
            match receiver.try_recv() {
                Ok(event) => incoming.push(event),
                Err(_) => break,
            }
        }

        let mut batch = Vec::with_capacity(incoming.len());
        for event in incoming {
            let event = match event {
                MarketEvent::Tick { tick } => {
                    let tick = normalizer::normalize(tick, stale_after_ms);
                    if !dedupe.should_emit(&tick) {
                        debug!(
                            venue = tick.instrument.venue_instance_id,
                            instrument = tick.instrument.instrument_id,
                            "duplicate BBO skipped"
                        );
                        continue;
                    }
                    MarketEvent::Tick { tick }
                }
                MarketEvent::VenueReset { venue_instance_id } => {
                    dedupe.reset_venue(&venue_instance_id);
                    MarketEvent::VenueReset { venue_instance_id }
                }
                catalog => catalog,
            };
            batch.push(event);
        }

        let batch = Arc::new(batch);
        let wal_writer = wal.clone();
        let wal_batch = batch.clone();
        task::spawn_blocking(move || wal_writer.append_batch(&wal_batch))
            .await
            .context("join WAL batch append")??;
        for event in batch.iter() {
            store.apply(event).await;
        }
    }
    Ok(())
}
