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

use super::parser;

#[derive(Debug, Clone)]
pub struct LighterAdapter {
    venue_instance_id: String,
    url: String,
    catalog: CatalogIndex,
    channel: String,
}

impl LighterAdapter {
    pub fn from_config(config: &VenueConfig) -> Self {
        Self {
            venue_instance_id: config.venue_instance_id.clone(),
            url: config
                .url
                .clone()
                .unwrap_or_else(|| "wss://mainnet.zklighter.elliot.ai/stream".to_string()),
            catalog: CatalogIndex::new(config.catalog()),
            channel: config
                .channel
                .clone()
                .unwrap_or_else(|| "ticker".to_string()),
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
            .context("send Lighter catalog")?;
        }

        let (stream, _) = ws::connect(&self.url).await?;
        let (mut write, mut read) = stream.split();

        for instrument in self.catalog.instruments() {
            let feed_key = instrument.feed_key();
            let payload = json!({
                "type": "subscribe",
                "channel": format!("{}/{}", self.channel, feed_key),
            });
            write
                .send(Message::Text(payload.to_string()))
                .await
                .with_context(|| format!("subscribe Lighter {feed_key}"))?;
        }

        info!(venue = %self.venue_instance_id, instruments = ?self.catalog.instruments(), "subscribed");
        let mut heartbeat = time::interval(Duration::from_secs(60));

        loop {
            tokio::select! {
                changed = shutdown.changed() => {
                    if changed.is_ok() && *shutdown.borrow() {
                        debug!(venue = "lighter", "shutdown received");
                        return Ok(());
                    }
                }
                _ = heartbeat.tick() => {
                    write.send(Message::Ping(Vec::new())).await?;
                }
                maybe_msg = read.next() => {
                    let Some(msg) = maybe_msg else {
                        anyhow::bail!("Lighter websocket closed");
                    };
                    match msg? {
                        Message::Text(text) => {
                            let recv_ts_ns = crate::ingest::time::unix_time_ns();
                            match parser::parse_message(&text, recv_ts_ns, &self.venue_instance_id) {
                                Ok(Some(tick)) => {
                                    if let Some(tick) = self.catalog.retarget_tick(tick) {
                                        tx.send(MarketEvent::Tick { tick }).await.context("send Lighter tick")?;
                                    }
                                }
                                Ok(None) => {}
                                Err(err) => {
                                    warn!(venue = "lighter", error = %err, payload = %text, "failed to parse websocket message");
                                }
                            }
                        }
                        Message::Close(frame) => {
                            anyhow::bail!("Lighter websocket closed: {frame:?}");
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
impl ExchangeAdapter for LighterAdapter {
    async fn run(
        &self,
        tx: Sender<MarketEvent>,
        shutdown: watch::Receiver<bool>,
    ) -> anyhow::Result<()> {
        run_with_reconnect("lighter", tx, shutdown, |tx, shutdown| {
            self.run_once(tx, shutdown)
        })
        .await
    }
}
