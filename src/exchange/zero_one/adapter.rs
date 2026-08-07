use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use anyhow::Context;
use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt, future::try_join_all};
use tokio::{
    sync::{mpsc::Sender, watch},
    time,
};
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, info, warn};

use crate::{
    config::VenueConfig,
    domain::{BboTick, InstrumentCatalog, MarketEvent},
    exchange::{
        CatalogIndex, ExchangeAdapter, LatestTickQueue, TICK_FLUSH_INTERVAL, run_with_reconnect,
    },
    ingest::ws,
};

use super::{
    orderbook::{ApplyResult, ZeroOneBooks},
    parser::{self, OrderbookSnapshot},
};

#[derive(Debug, Clone)]
pub struct ZeroOneAdapter {
    venue_instance_id: String,
    url: String,
    rest_url: String,
    channel: String,
    catalog: CatalogIndex,
    learned_batch_size: Arc<AtomicUsize>,
}

impl ZeroOneAdapter {
    pub fn from_config(config: &VenueConfig) -> Self {
        let url = config
            .url
            .clone()
            .unwrap_or_else(|| "wss://zo-mainnet.n1.xyz".to_string());
        Self {
            venue_instance_id: config.venue_instance_id.clone(),
            rest_url: derive_rest_url(&url),
            url,
            channel: config
                .channel
                .clone()
                .unwrap_or_else(|| "deltas".to_string()),
            catalog: CatalogIndex::new(config.catalog()),
            learned_batch_size: Arc::new(AtomicUsize::new(usize::MAX)),
        }
    }

    async fn run_once(
        &self,
        tx: Sender<MarketEvent>,
        shutdown: watch::Receiver<bool>,
    ) -> anyhow::Result<()> {
        for instrument in self.catalog.instruments() {
            tx.send(MarketEvent::Catalog {
                instrument: instrument.clone(),
            })
            .await
            .context("send 01 catalog")?;
        }

        let markets = self.catalog.instruments();
        if markets.is_empty() {
            return Ok(());
        }

        let mut batch_size = self
            .learned_batch_size
            .load(Ordering::Relaxed)
            .min(markets.len())
            .max(1);
        loop {
            let connections = subscription_batches(markets, batch_size).enumerate().map(
                |(connection_index, markets)| {
                    self.run_connection(connection_index, markets, tx.clone(), shutdown.clone())
                },
            );

            match try_join_all(connections).await {
                Ok(_) => return Ok(()),
                Err(error) if is_too_many_subscriptions(&error) && batch_size > 1 => {
                    let previous_batch_size = batch_size;
                    batch_size = reduced_batch_size(batch_size);
                    self.learned_batch_size.store(batch_size, Ordering::Relaxed);
                    warn!(
                        venue = %self.venue_instance_id,
                        previous_batch_size,
                        batch_size,
                        "exchange rejected combined streams; retrying with smaller adaptive batches"
                    );
                }
                Err(error) => return Err(error),
            }
        }
    }

    async fn run_connection(
        &self,
        connection_index: usize,
        markets: &[InstrumentCatalog],
        tx: Sender<MarketEvent>,
        mut shutdown: watch::Receiver<bool>,
    ) -> anyhow::Result<()> {
        let client = reqwest::Client::new();
        let mut books = ZeroOneBooks::default();
        let markets_by_symbol = markets
            .iter()
            .map(|market| (market.feed_key().to_string(), market.clone()))
            .collect::<HashMap<_, _>>();

        let ws_url = build_ws_url(&self.url, &self.channel, markets);
        let (stream, _) = ws::connect(&ws_url).await?;
        let (mut write, mut read) = stream.split();
        let mut pending_ticks = LatestTickQueue::default();

        let mut initialized_instruments = 0;
        for market in markets {
            let recv_ts_ns = crate::ingest::time::unix_time_ns();
            let tick = self
                .fetch_snapshot_tick(&client, &mut books, market, recv_ts_ns)
                .await
                .with_context(|| format!("fetch initial 01 snapshot {}", market.feed_key()))?;
            if let Some(tick) = tick {
                pending_ticks.push(&tx, tick)?;
                initialized_instruments += 1;
            } else {
                warn!(
                    venue = %self.venue_instance_id,
                    market = %market.feed_key(),
                    market_id = %market.instrument_id,
                    "01 market has no CLOB orderbook snapshot; skipping market"
                );
            }
        }

        info!(
            venue = %self.venue_instance_id,
            connection_index,
            instruments = initialized_instruments,
            url = %ws_url,
            "subscribed"
        );
        let mut heartbeat = time::interval(Duration::from_secs(30));
        let mut pending_flush = time::interval(TICK_FLUSH_INTERVAL);
        pending_flush.set_missed_tick_behavior(time::MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                changed = shutdown.changed() => {
                    if changed.is_ok() && *shutdown.borrow() {
                        debug!(venue = "01", "shutdown received");
                        return Ok(());
                    }
                }
                _ = heartbeat.tick() => {
                    write.send(Message::Ping(Vec::new())).await?;
                }
                _ = pending_flush.tick(), if !pending_ticks.is_empty() => {
                    pending_ticks.flush(&tx)?;
                }
                maybe_msg = read.next() => {
                    let Some(msg) = maybe_msg else {
                        anyhow::bail!("01 websocket closed");
                    };
                    match msg? {
                        Message::Text(text) => {
                            let recv_ts_ns = crate::ingest::time::unix_time_ns();
                            match parser::parse_delta(&text) {
                                Ok(Some(delta)) => {
                                    let market_symbol = delta.market_symbol.clone();
                                    match books.apply_delta(delta, recv_ts_ns) {
                                        ApplyResult::Tick(tick) => {
                                            pending_ticks.push(&tx, *tick)?;
                                        }
                                        ApplyResult::Skipped => {}
                                        ApplyResult::Gap { expected_last_update_id, received_last_update_id, .. } => {
                                            warn!(
                                                venue = "01",
                                                market = %market_symbol,
                                                expected_last_update_id,
                                                received_last_update_id,
                                                "orderbook delta gap; refreshing snapshot"
                                            );
                                            if let Some(market) = markets_by_symbol.get(&market_symbol) {
                                                let tick = self
                                                    .fetch_snapshot_tick(&client, &mut books, market, recv_ts_ns)
                                                    .await
                                                    .with_context(|| format!("refresh 01 snapshot {market_symbol}"))?;
                                                if let Some(mut tick) = tick {
                                                    tick.quality.gap = true;
                                                    tick.quality.add_note("orderbook delta gap; snapshot refreshed");
                                                    pending_ticks.push(&tx, tick)?;
                                                } else {
                                                    warn!(
                                                        venue = %self.venue_instance_id,
                                                        market = %market_symbol,
                                                        market_id = %market.instrument_id,
                                                        "01 gap refresh skipped because market has no CLOB snapshot"
                                                    );
                                                }
                                            }
                                        }
                                    }
                                }
                                Ok(None) => {}
                                Err(err) => {
                                    warn!(venue = "01", error = %err, payload = %text, "failed to parse websocket message");
                                }
                            }
                        }
                        Message::Close(frame) => {
                            anyhow::bail!("01 websocket closed: {frame:?}");
                        }
                        Message::Ping(payload) => {
                            write.send(Message::Pong(payload)).await?;
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    async fn fetch_snapshot_tick(
        &self,
        client: &reqwest::Client,
        books: &mut ZeroOneBooks,
        market: &InstrumentCatalog,
        recv_ts_ns: i128,
    ) -> anyhow::Result<Option<BboTick>> {
        let Some(snapshot) = self.fetch_snapshot(client, &market.instrument_id).await? else {
            return Ok(None);
        };
        Ok(Some(books.apply_snapshot(
            market.feed_key(),
            market.instrument_ref(),
            snapshot,
            recv_ts_ns,
        )))
    }

    async fn fetch_snapshot(
        &self,
        client: &reqwest::Client,
        market_id: &str,
    ) -> anyhow::Result<Option<OrderbookSnapshot>> {
        let url = format!("{}/market/{market_id}/orderbook", self.rest_url);
        let response = client
            .get(&url)
            .send()
            .await
            .with_context(|| format!("request 01 orderbook snapshot {url}"))?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let text = response
            .error_for_status()
            .with_context(|| format!("01 orderbook snapshot returned error status {url}"))?
            .text()
            .await
            .with_context(|| format!("read 01 orderbook snapshot {url}"))?;
        parser::parse_snapshot(&text).map(Some)
    }
}

#[async_trait]
impl ExchangeAdapter for ZeroOneAdapter {
    async fn run(
        &self,
        tx: Sender<MarketEvent>,
        shutdown: watch::Receiver<bool>,
    ) -> anyhow::Result<()> {
        run_with_reconnect("01", tx, shutdown, |tx, shutdown| {
            self.run_once(tx, shutdown)
        })
        .await
    }
}

fn derive_rest_url(ws_url: &str) -> String {
    let https = ws_url
        .trim_end_matches('/')
        .replacen("wss://", "https://", 1)
        .replacen("ws://", "http://", 1);
    https
        .strip_suffix("/ws")
        .unwrap_or(&https)
        .trim_end_matches('/')
        .to_string()
}

fn build_ws_url(base_url: &str, channel: &str, markets: &[InstrumentCatalog]) -> String {
    let base = base_url.trim_end_matches('/');
    let base = if base.ends_with("/ws") {
        base.to_string()
    } else {
        format!("{base}/ws")
    };
    let streams = markets
        .iter()
        .map(|market| format!("{channel}@{}", market.feed_key()))
        .collect::<Vec<_>>()
        .join("&");
    format!("{base}/{streams}")
}

fn subscription_batches(
    markets: &[InstrumentCatalog],
    batch_size: usize,
) -> std::slice::Chunks<'_, InstrumentCatalog> {
    markets.chunks(batch_size.max(1))
}

fn reduced_batch_size(current: usize) -> usize {
    current.div_ceil(2).max(1)
}

fn is_too_many_subscriptions(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .to_string()
            .to_ascii_lowercase()
            .contains("too many subscriptions")
    })
}

#[cfg(test)]
mod tests {
    use super::{
        build_ws_url, derive_rest_url, is_too_many_subscriptions, reduced_batch_size,
        subscription_batches,
    };
    use crate::domain::{InstrumentCatalog, ProductType};

    fn instrument(id: &str, feed: &str, base: &str) -> InstrumentCatalog {
        InstrumentCatalog::new(
            "01",
            id,
            feed,
            Some(feed.to_string()),
            ProductType::Perp,
            base,
            "USD",
            "USD",
            "USD",
            None,
            None,
            None,
            "active",
            None,
        )
    }

    #[test]
    fn derives_rest_url_from_ws_url() {
        assert_eq!(
            derive_rest_url("wss://zo-mainnet.n1.xyz/ws"),
            "https://zo-mainnet.n1.xyz"
        );
        assert_eq!(
            derive_rest_url("wss://zo-mainnet.n1.xyz"),
            "https://zo-mainnet.n1.xyz"
        );
    }

    #[test]
    fn builds_combined_delta_stream_url() {
        let markets = vec![
            instrument("0", "BTCUSD", "BTC"),
            instrument("1", "ETHUSD", "ETH"),
        ];
        assert_eq!(
            build_ws_url("wss://zo-mainnet.n1.xyz", "deltas", &markets),
            "wss://zo-mainnet.n1.xyz/ws/deltas@BTCUSD&deltas@ETHUSD"
        );
    }

    #[test]
    fn splits_catalogs_using_the_learned_batch_size() {
        let markets = (0..25)
            .map(|index| {
                instrument(
                    &index.to_string(),
                    &format!("ASSET{index}USD"),
                    &format!("ASSET{index}"),
                )
            })
            .collect::<Vec<_>>();

        let batch_sizes = subscription_batches(&markets, 13)
            .map(<[_]>::len)
            .collect::<Vec<_>>();

        assert_eq!(batch_sizes, vec![13, 12]);
    }

    #[test]
    fn halves_rejected_batch_sizes_without_a_fixed_exchange_limit() {
        assert_eq!(reduced_batch_size(25), 13);
        assert_eq!(reduced_batch_size(13), 7);
        assert_eq!(reduced_batch_size(2), 1);
        assert_eq!(reduced_batch_size(1), 1);
    }

    #[test]
    fn recognizes_the_exchange_subscription_limit_error() {
        let error = anyhow::anyhow!(
            "01 websocket closed: Some(CloseFrame {{ reason: \"subscribe: too many subscriptions\" }})"
        );

        assert!(is_too_many_subscriptions(&error));
        assert!(!is_too_many_subscriptions(&anyhow::anyhow!(
            "connection reset"
        )));
    }
}
