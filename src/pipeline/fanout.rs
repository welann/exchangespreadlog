use std::sync::Arc;

use tokio::sync::mpsc::Receiver;
use tracing::{debug, warn};

use crate::{
    domain::MarketEvent,
    pipeline::{dedupe::DedupeFilter, normalizer},
    state::SharedBboState,
    storage::BboSink,
};

pub async fn run_pipeline(
    mut rx: Receiver<MarketEvent>,
    sink: Arc<dyn BboSink>,
    state: SharedBboState,
    stale_after_ms: i64,
) -> anyhow::Result<()> {
    let mut dedupe = DedupeFilter::default();

    while let Some(event) = rx.recv().await {
        match event {
            MarketEvent::Catalog { instrument } => {
                if let Ok(mut state) = state.write() {
                    state.update_catalog(instrument.clone());
                } else {
                    warn!("failed to acquire BBO state write lock");
                }

                if let Err(err) = sink.write_catalog(&instrument).await {
                    warn!(error = %err, "failed to write instrument catalog");
                }
            }
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

                if let Ok(mut state) = state.write() {
                    state.update_tick(tick.clone());
                } else {
                    warn!("failed to acquire BBO state write lock");
                }

                if let Err(err) = sink.write_tick(&tick).await {
                    warn!(error = %err, "failed to write BBO tick");
                }
            }
            MarketEvent::VenueReset { venue_instance_id } => {
                dedupe.reset_venue(&venue_instance_id);
                if let Ok(mut state) = state.write() {
                    state.reset_venue(&venue_instance_id);
                } else {
                    warn!(venue = %venue_instance_id, "failed to acquire BBO state write lock");
                }
            }
        }
    }

    sink.flush().await
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tokio::sync::mpsc;

    use crate::{
        domain::{BboTick, InstrumentCatalog, MarketEvent, ProductType, QuoteRateBook, SourceKind},
        state::new_shared_state,
        storage::noop::NoopSink,
    };

    use super::run_pipeline;

    #[tokio::test]
    async fn venue_reset_clears_live_catalog_and_tick_state() {
        let catalog = InstrumentCatalog::new(
            "lighter",
            "1",
            "BTC",
            Some("1".to_string()),
            ProductType::Perp,
            "BTC",
            "USDC",
            "USDC",
            "USDC",
            None,
            None,
            None,
            "active",
            None,
        );
        let tick = BboTick::new(
            catalog.instrument_ref(),
            123,
            None,
            None,
            None,
            None,
            SourceKind::Ticker,
        );
        let state = new_shared_state(QuoteRateBook::default());
        let (tx, rx) = mpsc::channel(4);
        let pipeline = tokio::spawn(run_pipeline(rx, Arc::new(NoopSink), state.clone(), 0));

        tx.send(MarketEvent::Catalog {
            instrument: catalog,
        })
        .await
        .unwrap();
        tx.send(MarketEvent::Tick { tick }).await.unwrap();
        tx.send(MarketEvent::VenueReset {
            venue_instance_id: "lighter".to_string(),
        })
        .await
        .unwrap();
        drop(tx);
        pipeline.await.unwrap().unwrap();

        let snapshot = state.read().unwrap().snapshot();
        assert!(snapshot.catalogs.is_empty());
        assert!(snapshot.ticks.is_empty());
    }
}
