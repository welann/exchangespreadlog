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
        }
    }

    sink.flush().await
}
