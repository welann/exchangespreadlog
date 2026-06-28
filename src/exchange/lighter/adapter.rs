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
    config::VenueConfig, domain::BboTick, exchange::ExchangeAdapter, ingest::supervisor::Backoff,
    ingest::ws,
};

use super::parser;

#[derive(Debug, Clone)]
pub struct LighterAdapter {
    url: String,
    markets: Vec<String>,
    channel: String,
}

impl LighterAdapter {
    pub fn from_config(config: &VenueConfig) -> Self {
        Self {
            url: config
                .url
                .clone()
                .unwrap_or_else(|| "wss://mainnet.zklighter.elliot.ai/stream".to_string()),
            markets: config.markets.clone(),
            channel: config
                .channel
                .clone()
                .unwrap_or_else(|| "ticker".to_string()),
        }
    }

    async fn run_once(
        &self,
        tx: Sender<BboTick>,
        mut shutdown: watch::Receiver<bool>,
    ) -> anyhow::Result<()> {
        let (stream, _) = ws::connect(&self.url).await?;
        let (mut write, mut read) = stream.split();

        for market in &self.markets {
            let payload = json!({
                "type": "subscribe",
                "channel": format!("{}/{}", self.channel, market),
            });
            write
                .send(Message::Text(payload.to_string()))
                .await
                .with_context(|| format!("subscribe Lighter {market}"))?;
        }

        info!(venue = "lighter", markets = ?self.markets, "subscribed");
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
                            match parser::parse_message(&text, recv_ts_ns) {
                                Ok(Some(tick)) => {
                                    tx.send(tick).await.context("send Lighter tick")?;
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
        tx: Sender<BboTick>,
        shutdown: watch::Receiver<bool>,
    ) -> anyhow::Result<()> {
        let mut backoff = Backoff::default();
        while !*shutdown.borrow() {
            match self.run_once(tx.clone(), shutdown.clone()).await {
                Ok(()) => return Ok(()),
                Err(err) => {
                    let sleep = backoff.next_delay();
                    warn!(venue = "lighter", error = %err, ?sleep, "adapter restarting");
                    time::sleep(sleep).await;
                }
            }
        }
        Ok(())
    }
}
