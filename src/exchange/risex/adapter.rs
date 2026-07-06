use std::time::Duration;

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
    config::VenueConfig,
    domain::MarketEvent,
    exchange::{CatalogIndex, ExchangeAdapter, run_with_reconnect},
    ingest::ws,
};

use super::{
    orderbook::RisexBooks,
    parser::{self, ParsedMessage},
};

#[derive(Debug, Clone)]
pub struct RisexAdapter {
    venue_instance_id: String,
    url: String,
    catalog: CatalogIndex,
}

impl RisexAdapter {
    pub fn from_config(config: &VenueConfig) -> Self {
        Self {
            venue_instance_id: config.venue_instance_id.clone(),
            url: config
                .url
                .clone()
                .unwrap_or_else(|| "wss://ws.rise.trade/ws".to_string()),
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
            .context("send RiseX catalog")?;
        }

        let (stream, _) = ws::connect(&self.url).await?;
        let (mut write, mut read) = stream.split();
        let market_ids = self
            .catalog
            .instruments()
            .iter()
            .map(|market| {
                market.instrument_id.parse::<u64>().with_context(|| {
                    format!("RiseX market id must be numeric: {}", market.instrument_id)
                })
            })
            .collect::<anyhow::Result<Vec<_>>>()?;

        let payload = json!({
            "method": "subscribe",
            "params": {
                "channel": "orderbook",
                "market_ids": market_ids,
            }
        });
        write
            .send(Message::Text(payload.to_string()))
            .await
            .context("subscribe RiseX orderbook")?;

        info!(venue = %self.venue_instance_id, instruments = ?self.catalog.instruments(), "subscribed");
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
                                    let configured_instrument = self.catalog.resolve(&delta.market_id);
                                    match books.apply(delta, recv_ts_ns, configured_instrument) {
                                        Ok(Some(tick)) => {
                                            tx.send(MarketEvent::Tick { tick }).await.context("send RiseX tick")?;
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
        tx: Sender<MarketEvent>,
        shutdown: watch::Receiver<bool>,
    ) -> anyhow::Result<()> {
        run_with_reconnect("risex", tx, shutdown, |tx, shutdown| {
            self.run_once(tx, shutdown)
        })
        .await
    }
}
