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
    config::VenueConfig, domain::BboTick, exchange::ExchangeAdapter, ingest::supervisor::Backoff,
    ingest::ws,
};

use super::{
    orderbook::{ApplyResult, ZeroOneBooks},
    parser::{self, OrderbookSnapshot},
};

#[derive(Debug, Clone)]
pub struct ZeroOneAdapter {
    url: String,
    rest_url: String,
    channel: String,
    markets: Vec<ZeroOneMarket>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ZeroOneMarket {
    id: String,
    label: String,
    feed_symbol: String,
}

impl ZeroOneAdapter {
    pub fn from_config(config: &VenueConfig) -> Self {
        let url = config
            .url
            .clone()
            .unwrap_or_else(|| "wss://zo-mainnet.n1.xyz".to_string());
        Self {
            rest_url: derive_rest_url(&url),
            url,
            channel: config
                .channel
                .clone()
                .unwrap_or_else(|| "deltas".to_string()),
            markets: config
                .markets
                .iter()
                .map(|market| parse_market(market))
                .collect(),
        }
    }

    async fn run_once(
        &self,
        tx: Sender<BboTick>,
        mut shutdown: watch::Receiver<bool>,
    ) -> anyhow::Result<()> {
        let client = reqwest::Client::new();
        let mut books = ZeroOneBooks::default();
        let markets_by_symbol = self
            .markets
            .iter()
            .map(|market| (market.feed_symbol.clone(), market.clone()))
            .collect::<HashMap<_, _>>();

        let ws_url = build_ws_url(&self.url, &self.channel, &self.markets);
        let (stream, _) = ws::connect(&ws_url).await?;
        let (mut write, mut read) = stream.split();

        for market in &self.markets {
            let recv_ts_ns = crate::ingest::time::unix_time_ns();
            let tick = self
                .fetch_snapshot_tick(&client, &mut books, market, recv_ts_ns)
                .await
                .with_context(|| format!("fetch initial 01 snapshot {}", market.feed_symbol))?;
            tx.send(tick).await.context("send initial 01 tick")?;
        }

        info!(venue = "01", markets = ?self.markets, url = %ws_url, "subscribed");
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
                                            tx.send(tick).await.context("send 01 tick")?;
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
                                                tx.send(tick).await.context("send refreshed 01 tick")?;
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
        market: &ZeroOneMarket,
        recv_ts_ns: i128,
    ) -> anyhow::Result<BboTick> {
        let snapshot = self.fetch_snapshot(client, &market.id).await?;
        Ok(books.apply_snapshot(
            &market.feed_symbol,
            &market.id,
            &market.label,
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
        tx: Sender<BboTick>,
        shutdown: watch::Receiver<bool>,
    ) -> anyhow::Result<()> {
        let mut backoff = Backoff::default();
        while !*shutdown.borrow() {
            match self.run_once(tx.clone(), shutdown.clone()).await {
                Ok(()) => return Ok(()),
                Err(err) => {
                    let sleep = backoff.next_delay();
                    warn!(venue = "01", error = %err, ?sleep, "adapter restarting");
                    time::sleep(sleep).await;
                }
            }
        }
        Ok(())
    }
}

fn parse_market(raw: &str) -> ZeroOneMarket {
    let mut parts = raw.split([':', '=']).map(str::trim);
    let id = parts.next().unwrap_or_default().to_string();
    let label = parts.next().filter(|value| !value.is_empty());
    let feed_symbol = parts.next().filter(|value| !value.is_empty());

    let label = label
        .map(str::to_string)
        .unwrap_or_else(|| label_from_feed_symbol(feed_symbol.unwrap_or(&id)));
    let feed_symbol = feed_symbol
        .map(str::to_string)
        .unwrap_or_else(|| format!("{label}USD"));

    ZeroOneMarket {
        id,
        label,
        feed_symbol,
    }
}

fn label_from_feed_symbol(symbol: &str) -> String {
    symbol
        .strip_suffix("USD")
        .or_else(|| symbol.strip_suffix("USDC"))
        .unwrap_or(symbol)
        .to_string()
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

fn build_ws_url(base_url: &str, channel: &str, markets: &[ZeroOneMarket]) -> String {
    let base = base_url.trim_end_matches('/');
    let base = if base.ends_with("/ws") {
        base.to_string()
    } else {
        format!("{base}/ws")
    };
    let streams = markets
        .iter()
        .map(|market| format!("{channel}@{}", market.feed_symbol))
        .collect::<Vec<_>>()
        .join("&");
    format!("{base}/{streams}")
}

#[cfg(test)]
mod tests {
    use super::{build_ws_url, derive_rest_url, parse_market};

    #[test]
    fn parses_market_id_label_and_feed_symbol() {
        let market = parse_market("0:BTC:BTCUSD");
        assert_eq!(market.id, "0");
        assert_eq!(market.label, "BTC");
        assert_eq!(market.feed_symbol, "BTCUSD");
    }

    #[test]
    fn derives_feed_symbol_from_label() {
        let market = parse_market("2:SOL");
        assert_eq!(market.id, "2");
        assert_eq!(market.label, "SOL");
        assert_eq!(market.feed_symbol, "SOLUSD");
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
        let markets = vec![parse_market("0:BTC:BTCUSD"), parse_market("1:ETH:ETHUSD")];
        assert_eq!(
            build_ws_url("wss://zo-mainnet.n1.xyz", "deltas", &markets),
            "wss://zo-mainnet.n1.xyz/ws/deltas@BTCUSD&deltas@ETHUSD"
        );
    }
}
