use std::{collections::HashMap, time::Duration};

use anyhow::Context;
use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use serde_json::json;
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
    orderbook::RisexBooks,
    parser::{self, ParsedMessage},
};

#[derive(Debug, Clone)]
pub struct RisexAdapter {
    url: String,
    markets: Vec<RisexMarket>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RisexMarket {
    id: String,
    symbol: Option<String>,
}

impl RisexAdapter {
    pub fn from_config(config: &VenueConfig) -> Self {
        Self {
            url: config
                .url
                .clone()
                .unwrap_or_else(|| "wss://ws.rise.trade/ws".to_string()),
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
        let (stream, _) = ws::connect(&self.url).await?;
        let (mut write, mut read) = stream.split();
        let market_ids = self
            .markets
            .iter()
            .map(|market| {
                market
                    .id
                    .parse::<u64>()
                    .with_context(|| format!("RiseX market id must be numeric: {}", market.id))
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        let symbols = self
            .markets
            .iter()
            .filter_map(|market| {
                market
                    .symbol
                    .as_deref()
                    .map(|symbol| (market.id.clone(), symbol.to_string()))
            })
            .collect::<HashMap<_, _>>();

        let payload = if market_ids.is_empty() {
            json!({
                "method": "subscribe",
                "params": {
                    "channel": "orderbook",
                }
            })
        } else {
            json!({
                "method": "subscribe",
                "params": {
                    "channel": "orderbook",
                    "market_ids": market_ids,
                }
            })
        };
        write
            .send(Message::Text(payload.to_string()))
            .await
            .context("subscribe RiseX orderbook")?;

        info!(venue = "risex", markets = ?self.markets, "subscribed");
        let mut books = RisexBooks::default();
        let mut heartbeat = time::interval(Duration::from_secs(15));

        loop {
            tokio::select! {
                changed = shutdown.changed() => {
                    if changed.is_ok() && *shutdown.borrow() {
                        debug!(venue = "risex", "shutdown received");
                        return Ok(());
                    }
                }
                _ = heartbeat.tick() => {
                    write.send(Message::Text(json!({"op":"ping"}).to_string())).await?;
                }
                maybe_msg = read.next() => {
                    let Some(msg) = maybe_msg else {
                        anyhow::bail!("RiseX websocket closed");
                    };
                    match msg? {
                        Message::Text(text) => {
                            let recv_ts_ns = crate::ingest::time::unix_time_ns();
                            match parser::parse_message(&text) {
                                Ok(ParsedMessage::Orderbook(delta)) => {
                                    let configured_symbol = symbols.get(&delta.market_id).map(String::as_str);
                                    match books.apply(delta, recv_ts_ns, configured_symbol) {
                                        Ok(Some(tick)) => {
                                            tx.send(tick).await.context("send RiseX tick")?;
                                        }
                                        Ok(None) => {}
                                        Err(err) => {
                                            anyhow::bail!("RiseX orderbook state error: {err}");
                                        }
                                    }
                                }
                                Ok(ParsedMessage::JsonPing) => {
                                    write.send(Message::Text(json!({"type":"pong"}).to_string())).await?;
                                }
                                Ok(ParsedMessage::Ignore) => {}
                                Err(err) => {
                                    warn!(venue = "risex", error = %err, payload = %text, "failed to parse websocket message");
                                }
                            }
                        }
                        Message::Close(frame) => {
                            anyhow::bail!("RiseX websocket closed: {frame:?}");
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
}

#[async_trait]
impl ExchangeAdapter for RisexAdapter {
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
                    warn!(venue = "risex", error = %err, ?sleep, "adapter restarting");
                    time::sleep(sleep).await;
                }
            }
        }
        Ok(())
    }
}

fn parse_market(raw: &str) -> RisexMarket {
    let raw = raw.trim();
    let (id, symbol) = raw
        .split_once(':')
        .or_else(|| raw.split_once('='))
        .map(|(id, symbol)| (id.trim(), Some(symbol.trim().to_string())))
        .unwrap_or((raw, None));

    RisexMarket {
        id: id.to_string(),
        symbol: symbol.filter(|symbol| !symbol.is_empty()),
    }
}

#[cfg(test)]
mod tests {
    use super::parse_market;

    #[test]
    fn parses_market_id_symbol_mapping() {
        let market = parse_market("1:BTC");
        assert_eq!(market.id, "1");
        assert_eq!(market.symbol.as_deref(), Some("BTC"));

        let market = parse_market("4=SOL");
        assert_eq!(market.id, "4");
        assert_eq!(market.symbol.as_deref(), Some("SOL"));
    }

    #[test]
    fn parses_plain_market_id() {
        let market = parse_market("2");
        assert_eq!(market.id, "2");
        assert_eq!(market.symbol, None);
    }
}
