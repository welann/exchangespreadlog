use std::sync::Arc;

use tokio::sync::mpsc::Receiver;
use tracing::{debug, warn};

use crate::{
    domain::BboTick,
    pipeline::{dedupe::DedupeFilter, normalizer},
    state::SharedBboState,
    storage::BboSink,
};

pub async fn run_pipeline(
    mut rx: Receiver<BboTick>,
    sink: Arc<dyn BboSink>,
    state: SharedBboState,
) -> anyhow::Result<()> {
    let mut dedupe = DedupeFilter::default();

    while let Some(tick) = rx.recv().await {
        let tick = normalizer::normalize(tick);
        if !dedupe.should_emit(&tick) {
            debug!(
                venue = tick.venue.as_str(),
                market = tick.market.label(),
                "duplicate BBO skipped"
            );
            continue;
        }

        if let Ok(mut state) = state.write() {
            state.update(tick.clone());
        } else {
            warn!("failed to acquire BBO state write lock");
        }

        if let Err(err) = sink.write(&tick).await {
            warn!(error = %err, "failed to write BBO tick");
        }
    }

    sink.flush().await
}
