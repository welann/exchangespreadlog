use std::{
    collections::{BTreeMap, HashMap},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use anyhow::{Context, bail};
use serde::Serialize;
use tokio::sync::{RwLock, broadcast};

use crate::{
    domain::{BboTick, InstrumentCatalog, MarketEvent, QuoteRateBook},
    ingest::time::unix_time_ns,
};

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LiveNotice {
    Catalog { instrument_key: String },
    Tick { instrument_key: String },
    VenueReset { venue: String },
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstrumentView {
    pub instrument_key: String,
    pub venue: String,
    pub instrument_id: String,
    pub symbol: String,
    pub base_asset: String,
    pub quote_asset: String,
    pub latest_recv_ms: Option<i64>,
    pub status: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MarketView {
    pub base_asset: String,
    pub quote_assets: Vec<String>,
    pub instruments: Vec<InstrumentView>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpreadPoint {
    pub ts_ms: i64,
    pub a_state_ts_ms: i64,
    pub b_state_ts_ms: i64,
    pub a_bid: f64,
    pub a_ask: f64,
    pub b_bid: f64,
    pub b_ask: f64,
    pub a_to_b: f64,
    pub b_to_a: f64,
    pub a_to_b_bp: f64,
    pub b_to_a_bp: f64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveSpread {
    pub state: &'static str,
    pub reason: Option<String>,
    pub target_quote: String,
    pub point: Option<SpreadPoint>,
    pub leg_a: InstrumentView,
    pub leg_b: InstrumentView,
}

#[derive(Debug, Clone)]
pub struct PairConversion {
    pub target_quote: String,
    pub a_rate: f64,
    pub b_rate: f64,
}

#[derive(Debug, Default)]
struct Inner {
    catalogs: HashMap<String, InstrumentCatalog>,
    ticks: HashMap<String, BboTick>,
    venue_connected: HashMap<String, bool>,
}

#[derive(Debug, Default)]
pub struct RuntimeStats {
    pub received_ticks: AtomicU64,
    pub accepted_ticks: AtomicU64,
    pub rejected_ticks: AtomicU64,
    pub venue_resets: AtomicU64,
}

#[derive(Clone)]
pub struct LiveBookStore {
    inner: Arc<RwLock<Inner>>,
    updates: broadcast::Sender<LiveNotice>,
    stats: Arc<RuntimeStats>,
    max_book_age_ms: i64,
    quote_rates: QuoteRateBook,
}

impl LiveBookStore {
    pub fn new(max_book_age_ms: i64, quote_rates: QuoteRateBook) -> Self {
        let (updates, _) = broadcast::channel(4_096);
        Self {
            inner: Arc::new(RwLock::new(Inner::default())),
            updates,
            stats: Arc::new(RuntimeStats::default()),
            max_book_age_ms,
            quote_rates,
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<LiveNotice> {
        self.updates.subscribe()
    }

    pub fn stats(&self) -> Arc<RuntimeStats> {
        self.stats.clone()
    }

    pub async fn apply(&self, event: &MarketEvent) {
        match event {
            MarketEvent::Catalog { instrument } => {
                let key = instrument.catalog_id.clone();
                self.inner
                    .write()
                    .await
                    .catalogs
                    .insert(key.clone(), instrument.clone());
                let _ = self.updates.send(LiveNotice::Catalog {
                    instrument_key: key,
                });
            }
            MarketEvent::Tick { tick } => {
                self.stats.received_ticks.fetch_add(1, Ordering::Relaxed);
                let valid = tick.bid.is_some()
                    && tick.ask.is_some()
                    && !tick.quality.gap
                    && !tick.quality.stale
                    && !tick.quality.inconsistent;
                if !valid {
                    self.stats.rejected_ticks.fetch_add(1, Ordering::Relaxed);
                    return;
                }
                self.stats.accepted_ticks.fetch_add(1, Ordering::Relaxed);
                let key = tick.instrument.catalog_id.clone();
                let mut inner = self.inner.write().await;
                inner
                    .venue_connected
                    .insert(tick.instrument.venue_instance_id.clone(), true);
                inner.ticks.insert(key.clone(), tick.clone());
                drop(inner);
                let _ = self.updates.send(LiveNotice::Tick {
                    instrument_key: key,
                });
            }
            MarketEvent::VenueReset { venue_instance_id } => {
                self.stats.venue_resets.fetch_add(1, Ordering::Relaxed);
                let mut inner = self.inner.write().await;
                inner
                    .catalogs
                    .retain(|_, catalog| catalog.venue_instance_id != *venue_instance_id);
                inner
                    .ticks
                    .retain(|_, tick| tick.instrument.venue_instance_id != *venue_instance_id);
                inner
                    .venue_connected
                    .insert(venue_instance_id.clone(), false);
                drop(inner);
                let _ = self.updates.send(LiveNotice::VenueReset {
                    venue: venue_instance_id.clone(),
                });
            }
        }
    }

    pub async fn markets(&self) -> Vec<MarketView> {
        let inner = self.inner.read().await;
        let mut grouped: BTreeMap<String, Vec<InstrumentView>> = BTreeMap::new();
        for catalog in inner
            .catalogs
            .values()
            .filter(|catalog| catalog.status == "active")
        {
            let tick = inner.ticks.get(&catalog.catalog_id);
            grouped
                .entry(catalog.base_asset.clone())
                .or_default()
                .push(instrument_view(catalog, tick));
        }

        grouped
            .into_iter()
            .filter_map(|(base_asset, mut instruments)| {
                instruments.sort_by(|left, right| left.venue.cmp(&right.venue));
                let distinct_venues = instruments
                    .iter()
                    .map(|instrument| instrument.venue.as_str())
                    .collect::<std::collections::HashSet<_>>()
                    .len();
                let quote_assets = instruments
                    .iter()
                    .map(|instrument| instrument.quote_asset.clone())
                    .collect::<std::collections::BTreeSet<_>>()
                    .into_iter()
                    .collect();
                (distinct_venues >= 2).then_some(MarketView {
                    base_asset,
                    quote_assets,
                    instruments,
                })
            })
            .collect()
    }

    pub async fn instrument(&self, key: &str) -> Option<InstrumentView> {
        let inner = self.inner.read().await;
        let catalog = inner.catalogs.get(key)?;
        Some(instrument_view(catalog, inner.ticks.get(key)))
    }

    pub async fn spread(&self, leg_a: &str, leg_b: &str) -> anyhow::Result<LiveSpread> {
        if leg_a == leg_b {
            bail!("choose two different instruments");
        }
        let now_ms = i64::try_from(unix_time_ns() / 1_000_000).context("clock overflow")?;
        let inner = self.inner.read().await;
        let catalog_a = inner
            .catalogs
            .get(leg_a)
            .context("leg A is not in the live catalog")?;
        let catalog_b = inner
            .catalogs
            .get(leg_b)
            .context("leg B is not in the live catalog")?;
        let conversion = ensure_comparable(&self.quote_rates, catalog_a, catalog_b)?;

        let view_a = instrument_view(catalog_a, inner.ticks.get(leg_a));
        let view_b = instrument_view(catalog_b, inner.ticks.get(leg_b));
        let Some(tick_a) = inner.ticks.get(leg_a) else {
            return Ok(unavailable(
                "leg A has no valid BBO",
                conversion.target_quote,
                view_a,
                view_b,
            ));
        };
        let Some(tick_b) = inner.ticks.get(leg_b) else {
            return Ok(unavailable(
                "leg B has no valid BBO",
                conversion.target_quote,
                view_a,
                view_b,
            ));
        };
        let age_a = now_ms - recv_ms(tick_a);
        let age_b = now_ms - recv_ms(tick_b);
        if age_a > self.max_book_age_ms || age_b > self.max_book_age_ms {
            return Ok(unavailable(
                "one or both books are stale",
                conversion.target_quote,
                view_a,
                view_b,
            ));
        }
        let point =
            spread_from_ticks_with_rates(tick_a, tick_b, conversion.a_rate, conversion.b_rate)?;
        Ok(LiveSpread {
            state: "valid",
            reason: None,
            target_quote: conversion.target_quote,
            point: Some(point),
            leg_a: view_a,
            leg_b: view_b,
        })
    }

    pub async fn catalog_pair(
        &self,
        leg_a: &str,
        leg_b: &str,
    ) -> anyhow::Result<(InstrumentCatalog, InstrumentCatalog, PairConversion)> {
        let inner = self.inner.read().await;
        let first = inner
            .catalogs
            .get(leg_a)
            .cloned()
            .context("leg A is not in the catalog")?;
        let second = inner
            .catalogs
            .get(leg_b)
            .cloned()
            .context("leg B is not in the catalog")?;
        let conversion = ensure_comparable(&self.quote_rates, &first, &second)?;
        Ok((first, second, conversion))
    }
}

pub fn spread_from_ticks(first: &BboTick, second: &BboTick) -> anyhow::Result<SpreadPoint> {
    spread_from_ticks_with_rates(first, second, 1.0, 1.0)
}

fn spread_from_ticks_with_rates(
    first: &BboTick,
    second: &BboTick,
    a_rate: f64,
    b_rate: f64,
) -> anyhow::Result<SpreadPoint> {
    let a_bid = first
        .bid
        .as_ref()
        .context("leg A bid is missing")?
        .price
        .to_f64()
        * a_rate;
    let a_ask = first
        .ask
        .as_ref()
        .context("leg A ask is missing")?
        .price
        .to_f64()
        * a_rate;
    let b_bid = second
        .bid
        .as_ref()
        .context("leg B bid is missing")?
        .price
        .to_f64()
        * b_rate;
    let b_ask = second
        .ask
        .as_ref()
        .context("leg B ask is missing")?
        .price
        .to_f64()
        * b_rate;
    let a_to_b = a_bid - b_ask;
    let b_to_a = b_bid - a_ask;
    Ok(SpreadPoint {
        ts_ms: recv_ms(first).max(recv_ms(second)),
        a_state_ts_ms: recv_ms(first),
        b_state_ts_ms: recv_ms(second),
        a_bid,
        a_ask,
        b_bid,
        b_ask,
        a_to_b,
        b_to_a,
        a_to_b_bp: a_to_b / b_ask * 10_000.0,
        b_to_a_bp: b_to_a / a_ask * 10_000.0,
    })
}

fn ensure_comparable(
    rates: &QuoteRateBook,
    first: &InstrumentCatalog,
    second: &InstrumentCatalog,
) -> anyhow::Result<PairConversion> {
    if first.base_asset != second.base_asset {
        bail!("BASE_ASSET_MISMATCH");
    }
    if first.venue_instance_id == second.venue_instance_id {
        bail!("choose instruments from different venues");
    }
    let target_quote = rates
        .common_quote(&first.quote_asset, &second.quote_asset)
        .context("QUOTE_CONVERSION_MISSING")?;
    let a_rate = rates
        .rate(&first.quote_asset, &target_quote)
        .context("QUOTE_CONVERSION_MISSING")?
        .to_f64();
    let b_rate = rates
        .rate(&second.quote_asset, &target_quote)
        .context("QUOTE_CONVERSION_MISSING")?
        .to_f64();
    Ok(PairConversion {
        target_quote,
        a_rate,
        b_rate,
    })
}

fn unavailable(
    reason: &str,
    target_quote: String,
    leg_a: InstrumentView,
    leg_b: InstrumentView,
) -> LiveSpread {
    LiveSpread {
        state: "unavailable",
        reason: Some(reason.to_string()),
        target_quote,
        point: None,
        leg_a,
        leg_b,
    }
}

fn instrument_view(catalog: &InstrumentCatalog, tick: Option<&BboTick>) -> InstrumentView {
    InstrumentView {
        instrument_key: catalog.catalog_id.clone(),
        venue: catalog.venue_instance_id.clone(),
        instrument_id: catalog.instrument_id.clone(),
        symbol: catalog.display_symbol().to_string(),
        base_asset: catalog.base_asset.clone(),
        quote_asset: catalog.quote_asset.clone(),
        latest_recv_ms: tick.map(recv_ms),
        status: catalog.status.clone(),
    }
}

fn recv_ms(tick: &BboTick) -> i64 {
    i64::try_from(tick.recv_ts_ns / 1_000_000).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use crate::domain::{
        BboTick, BestLevel, Fixed, InstrumentCatalog, MarketEvent, ProductType, QuoteRate,
        QuoteRateBook, SourceKind,
    };

    use super::{LiveBookStore, ensure_comparable};

    fn catalog(venue: &str, quote: &str) -> InstrumentCatalog {
        InstrumentCatalog::new(
            venue,
            "BTC",
            "BTC",
            Some("BTC".to_string()),
            ProductType::Perp,
            "BTC",
            quote,
            quote,
            quote,
            None,
            None,
            None,
            "active",
            None,
        )
    }

    #[test]
    fn resolves_cross_quote_pair_through_explicit_rate_book() {
        let rates = QuoteRateBook::new([QuoteRate {
            from: "USDC".to_string(),
            to: "USD".to_string(),
            rate: Fixed::new(1, 0),
        }]);

        let conversion =
            ensure_comparable(&rates, &catalog("lighter", "USDC"), &catalog("01", "USD")).unwrap();

        assert_eq!(conversion.target_quote, "USD");
        assert_eq!(conversion.a_rate, 1.0);
        assert_eq!(conversion.b_rate, 1.0);
    }

    #[test]
    fn rejects_cross_quote_pair_without_an_explicit_rate() {
        let error = ensure_comparable(
            &QuoteRateBook::default(),
            &catalog("lighter", "USDC"),
            &catalog("perpl", "AUSD"),
        )
        .unwrap_err();

        assert!(error.to_string().contains("QUOTE_CONVERSION_MISSING"));
    }

    #[tokio::test]
    async fn venue_reset_removes_only_that_venues_catalogs_and_ticks() {
        let store = LiveBookStore::new(30_000, QuoteRateBook::default());
        let lighter = catalog("lighter", "USDC");
        let hyperliquid = catalog("hyperliquid", "USDC");
        for instrument in [&lighter, &hyperliquid] {
            store
                .apply(&MarketEvent::Catalog {
                    instrument: instrument.clone(),
                })
                .await;
            store
                .apply(&MarketEvent::Tick {
                    tick: BboTick::new(
                        instrument.instrument_ref(),
                        123,
                        None,
                        None,
                        Some(BestLevel::new(Fixed::new(100, 0), Fixed::new(1, 0), None)),
                        Some(BestLevel::new(Fixed::new(101, 0), Fixed::new(1, 0), None)),
                        SourceKind::Bbo,
                    ),
                })
                .await;
        }

        store
            .apply(&MarketEvent::VenueReset {
                venue_instance_id: "lighter".to_string(),
            })
            .await;

        assert!(store.instrument(&lighter.catalog_id).await.is_none());
        assert!(store.instrument(&hyperliquid.catalog_id).await.is_some());
    }
}
