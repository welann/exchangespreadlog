use std::{collections::HashMap, time::Duration};

use anyhow::Context;
use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use tokio::{
    sync::{mpsc::Sender, watch},
    time,
};
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, info, warn};

use crate::{
    config::VenueConfig,
    domain::{BboTick, InstrumentCatalog, MarketEvent},
    exchange::{CatalogIndex, ExchangeAdapter, run_with_reconnect},
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
        }
    }

    async fn run_once(
        &self,
        tx: Sender<MarketEvent>,
        mut shutdown: watch::Receiver<bool>,
    ) -> anyhow::Result<()> {
        for instrument in self.catalog.instruments() {
            tx.send(MarketEvent::Catalog {
                instrument: instrument.clone(),
            })
            .await
            .context("send 01 catalog")?;
        }

        let client = reqwest::Client::new();
        let mut books = ZeroOneBooks::default();
        let markets_by_symbol = self
            .catalog
            .instruments()
            .iter()
            .map(|market| (market.feed_key().to_string(), market.clone()))
            .collect::<HashMap<_, _>>();

        let ws_url = build_ws_url(&self.url, &self.channel, self.catalog.instruments());
        let (stream, _) = ws::connect(&ws_url).await?;
        let (mut write, mut read) = stream.split();

        for market in self.catalog.instruments() {
            let recv_ts_ns = crate::ingest::time::unix_time_ns();
            let tick = self
                .fetch_snapshot_tick(&client, &mut books, market, recv_ts_ns)
                .await
                .with_context(|| format!("fetch initial 01 snapshot {}", market.feed_key()))?;
            tx.send(MarketEvent::Tick { tick })
                .await
                .context("send initial 01 tick")?;
        }

        info!(venue = %self.venue_instance_id, instruments = ?self.catalog.instruments(), url = %ws_url, "subscribed");
        let mut heartbeat = time::interval(Duration::from_secs(30));

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
                                            tx.send(MarketEvent::Tick { tick }).await.context("send 01 tick")?;
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
                                                let mut tick = self
                                                    .fetch_snapshot_tick(&client, &mut books, market, recv_ts_ns)
                                                    .await
                                                    .with_context(|| format!("refresh 01 snapshot {market_symbol}"))?;
                                                tick.quality.gap = true;
                                                tick.quality.add_note("orderbook delta gap; snapshot refreshed");
                                                tx.send(MarketEvent::Tick { tick }).await.context("send refreshed 01 tick")?;
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
    ) -> anyhow::Result<BboTick> {
        let snapshot = self.fetch_snapshot(client, &market.instrument_id).await?;
        Ok(books.apply_snapshot(
            market.feed_key(),
            market.instrument_ref(),
            snapshot,
            recv_ts_ns,
        ))
    }

    async fn fetch_snapshot(
        &self,
        client: &reqwest::Client,
        market_id: &str,
    ) -> anyhow::Result<OrderbookSnapshot> {
        let url = format!("{}/market/{market_id}/orderbook", self.rest_url);
        let text = client
            .get(&url)
            .send()
            .await
            .with_context(|| format!("request 01 orderbook snapshot {url}"))?
            .error_for_status()
            .with_context(|| format!("01 orderbook snapshot returned error status {url}"))?
            .text()
            .await
            .with_context(|| format!("read 01 orderbook snapshot {url}"))?;
        parser::parse_snapshot(&text)
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

#[cfg(test)]
mod tests {
    use super::{build_ws_url, derive_rest_url};
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
}
