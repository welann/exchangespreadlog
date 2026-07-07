pub mod ethereal;
pub mod hyperliquid;
pub mod lighter;
pub mod risex;
pub mod zero_one;

use std::{
    collections::HashMap,
    future::Future,
    time::{Duration, Instant},
};

use async_trait::async_trait;
use tokio::{
    sync::{mpsc::Sender, watch},
    time,
};
use tracing::{info, warn};

use crate::{
    domain::{BboTick, Fixed, InstrumentCatalog, InstrumentRef, MarketEvent},
    ingest::supervisor::Backoff,
};

const STABLE_CONNECTION_RESET_AFTER: Duration = Duration::from_secs(30);
const TRANSIENT_WS_RECONNECT_DELAY: Duration = Duration::from_secs(1);
const TRANSIENT_WS_RECONNECT_CAP: Duration = Duration::from_secs(5);

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
    run_once: F,
) -> anyhow::Result<()>
where
    F: FnMut(Sender<MarketEvent>, watch::Receiver<bool>) -> Fut + Send,
    Fut: Future<Output = anyhow::Result<()>> + Send,
{
    run_with_reconnect_backoff(venue, tx, shutdown, Backoff::default(), run_once).await
}

pub async fn run_with_reconnect_backoff<F, Fut>(
    venue: &'static str,
    tx: Sender<MarketEvent>,
    mut shutdown: watch::Receiver<bool>,
    mut backoff: Backoff,
    mut run_once: F,
) -> anyhow::Result<()>
where
    F: FnMut(Sender<MarketEvent>, watch::Receiver<bool>) -> Fut + Send,
    Fut: Future<Output = anyhow::Result<()>> + Send,
{
    while !*shutdown.borrow() {
        let started_at = Instant::now();
        match run_once(tx.clone(), shutdown.clone()).await {
            Ok(()) => return Ok(()),
            Err(err) => {
                let uptime = started_at.elapsed();
                let decision = reconnect_decision(&mut backoff, &err, uptime);
                log_adapter_restart(venue, &err, decision, uptime);
                tokio::select! {
                    _ = time::sleep(decision.sleep) => {}
                    changed = shutdown.changed() => {
                        if changed.is_ok() && *shutdown.borrow() {
                            return Ok(());
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ReconnectDecision {
    sleep: Duration,
    kind: ReconnectKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReconnectKind {
    TransientWebsocketDisconnect,
    Error,
}

fn reconnect_decision(
    backoff: &mut Backoff,
    err: &anyhow::Error,
    uptime: Duration,
) -> ReconnectDecision {
    let was_stable_connection = uptime >= STABLE_CONNECTION_RESET_AFTER;
    if was_stable_connection {
        backoff.reset();
    }

    if is_transient_websocket_disconnect(err) {
        let sleep = if was_stable_connection {
            TRANSIENT_WS_RECONNECT_DELAY
        } else {
            backoff.next_delay().min(TRANSIENT_WS_RECONNECT_CAP)
        };
        return ReconnectDecision {
            sleep,
            kind: ReconnectKind::TransientWebsocketDisconnect,
        };
    }

    ReconnectDecision {
        sleep: backoff.next_delay(),
        kind: ReconnectKind::Error,
    }
}

fn log_adapter_restart(
    venue: &str,
    err: &anyhow::Error,
    decision: ReconnectDecision,
    uptime: Duration,
) {
    if decision.kind == ReconnectKind::TransientWebsocketDisconnect {
        info!(
            venue,
            reason = %err,
            sleep = ?decision.sleep,
            ?uptime,
            "adapter reconnecting after transient websocket disconnect"
        );
    } else {
        warn!(
            venue,
            error = %err,
            sleep = ?decision.sleep,
            ?uptime,
            "adapter restarting"
        );
    }
}

fn is_transient_websocket_disconnect(err: &anyhow::Error) -> bool {
    let mut text = String::new();
    for cause in err.chain() {
        if !text.is_empty() {
            text.push_str(": ");
        }
        text.push_str(&cause.to_string());
    }
    let text = text.to_ascii_lowercase();

    text.contains("without closing handshake")
        || text.contains("connection reset without closing handshake")
        || text.contains("connection reset by peer")
        || text.contains("broken pipe")
}

#[derive(Debug, Clone)]
pub struct CatalogIndex {
    instruments: Vec<InstrumentCatalog>,
    refs_by_feed_key: HashMap<String, InstrumentRef>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogLookupMiss {
    pub venue_instance_id: String,
    pub feed_key: String,
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

    pub fn retarget_tick(&self, mut tick: BboTick) -> Result<BboTick, CatalogLookupMiss> {
        let feed_key = tick.instrument.instrument_id.clone();
        let instrument = self.resolve(&feed_key).ok_or_else(|| CatalogLookupMiss {
            venue_instance_id: tick.instrument.venue_instance_id.clone(),
            feed_key,
        })?;
        tick.instrument = instrument;
        Ok(tick)
    }
}

pub fn warn_catalog_miss(adapter: &str, miss: CatalogLookupMiss) {
    warn!(
        venue = %miss.venue_instance_id,
        adapter,
        feed_key = %miss.feed_key,
        "tick skipped because feed key is missing from instrument catalog"
    );
}

pub fn merge_configured_catalog(
    configured: Vec<InstrumentCatalog>,
    fetched: Vec<InstrumentCatalog>,
) -> Vec<InstrumentCatalog> {
    if configured.is_empty() {
        return fetched;
    }

    // Configured instruments define the subscription set; exchange metadata only enriches rules.
    let fetched_by_key = catalog_lookup(fetched);
    configured
        .into_iter()
        .map(|instrument| lookup_catalog(&fetched_by_key, &instrument).unwrap_or(instrument))
        .collect()
}

pub fn decimal_tick(decimals: u32) -> Option<Fixed> {
    Some(Fixed::new(1, decimals))
}

fn catalog_lookup(instruments: Vec<InstrumentCatalog>) -> HashMap<String, InstrumentCatalog> {
    let mut lookup = HashMap::new();
    for instrument in instruments {
        for key in catalog_keys(&instrument) {
            lookup.entry(key).or_insert_with(|| instrument.clone());
        }
    }
    lookup
}

fn lookup_catalog(
    lookup: &HashMap<String, InstrumentCatalog>,
    instrument: &InstrumentCatalog,
) -> Option<InstrumentCatalog> {
    catalog_keys(instrument)
        .into_iter()
        .find_map(|key| lookup.get(&key).cloned())
}

fn catalog_keys(instrument: &InstrumentCatalog) -> Vec<String> {
    let mut keys = vec![
        instrument.instrument_id.clone(),
        instrument.raw_symbol.clone(),
        instrument.display_symbol().to_string(),
    ];
    if let Some(feed_symbol) = &instrument.feed_symbol {
        keys.push(feed_symbol.clone());
    }
    keys.sort();
    keys.dedup();
    keys
}

#[cfg(test)]
mod tests {
    use super::{
        CatalogIndex, ReconnectDecision, ReconnectKind, TRANSIENT_WS_RECONNECT_DELAY, decimal_tick,
        is_transient_websocket_disconnect, merge_configured_catalog, reconnect_decision,
    };
    use std::time::Duration;

    use crate::{
        domain::{BboTick, BestLevel, InstrumentCatalog, ProductType, SourceKind},
        ingest::supervisor::Backoff,
    };

    fn catalog(id: &str, raw: &str, feed: &str, price_tick: Option<&str>) -> InstrumentCatalog {
        InstrumentCatalog::new(
            "lighter",
            id,
            raw,
            Some(feed.to_string()),
            ProductType::Perp,
            raw,
            "USDC",
            "USDC",
            "USDC",
            price_tick.map(|value| value.parse().unwrap()),
            None,
            None,
            "active",
            None,
        )
    }

    #[test]
    fn retarget_tick_reports_missing_catalog_key() {
        let index = CatalogIndex::new(vec![catalog("1", "BTC", "1", None)]);
        let tick = BboTick::new(
            crate::domain::InstrumentRef::unchecked("lighter", "999"),
            123,
            None,
            None,
            None::<BestLevel>,
            None::<BestLevel>,
            SourceKind::Ticker,
        );

        let miss = index.retarget_tick(tick).unwrap_err();
        assert_eq!(miss.venue_instance_id, "lighter");
        assert_eq!(miss.feed_key, "999");
    }

    #[test]
    fn merge_configured_catalog_keeps_subscription_order_and_uses_fetched_rules() {
        let configured = vec![
            catalog("1", "BTC", "1", None),
            catalog("2", "ETH", "2", None),
        ];
        let fetched = vec![catalog("2", "ETH", "2", Some("0.01"))];

        let merged = merge_configured_catalog(configured, fetched);

        assert_eq!(merged[0].instrument_id, "1");
        assert_eq!(merged[0].price_tick, None);
        assert_eq!(merged[1].instrument_id, "2");
        assert_eq!(merged[1].price_tick, decimal_tick(2));
    }

    #[test]
    fn classifies_connection_reset_as_transient_disconnect() {
        let err =
            anyhow::anyhow!("WebSocket protocol error: Connection reset without closing handshake");
        assert!(is_transient_websocket_disconnect(&err));

        let err = anyhow::anyhow!("Ethereal L2Book gap for BTCUSD");
        assert!(!is_transient_websocket_disconnect(&err));
    }

    #[test]
    fn reconnects_quickly_after_stable_transient_websocket_disconnect() {
        let err =
            anyhow::anyhow!("WebSocket protocol error: Connection reset without closing handshake");
        let mut backoff = Backoff::new(Duration::from_secs(15), Duration::from_secs(300));

        let decision = reconnect_decision(&mut backoff, &err, Duration::from_secs(120));

        assert_eq!(
            decision,
            ReconnectDecision {
                sleep: TRANSIENT_WS_RECONNECT_DELAY,
                kind: ReconnectKind::TransientWebsocketDisconnect,
            }
        );
    }

    #[test]
    fn caps_rapid_transient_websocket_disconnect_backoff() {
        let err =
            anyhow::anyhow!("WebSocket protocol error: Connection reset without closing handshake");
        let mut backoff = Backoff::new(Duration::from_secs(15), Duration::from_secs(300));

        let decision = reconnect_decision(&mut backoff, &err, Duration::from_secs(1));

        assert_eq!(
            decision,
            ReconnectDecision {
                sleep: Duration::from_secs(5),
                kind: ReconnectKind::TransientWebsocketDisconnect,
            }
        );
    }
}
