use std::time::Duration;

use anyhow::Context;
use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::json;
use tokio::{
    sync::{mpsc::Sender, watch},
    time,
};
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, info, warn};

use crate::{
    config::{CatalogSource, VenueConfig},
    domain::{InstrumentCatalog, MarketEvent, ProductType},
    exchange::{
        CatalogIndex, ExchangeAdapter, decimal_tick, merge_configured_catalog, run_with_reconnect,
        warn_catalog_miss,
    },
    ingest::ws,
};

use super::parser;

#[derive(Debug, Clone)]
pub struct HyperliquidAdapter {
    venue_instance_id: String,
    url: String,
    configured_catalog: Vec<InstrumentCatalog>,
    catalog_source: CatalogSource,
    metadata_url: Option<String>,
    default_quote_asset: String,
    default_settle_asset: String,
    default_margin_asset: String,
    channel: String,
}

impl HyperliquidAdapter {
    pub fn from_config(config: &VenueConfig) -> Self {
        Self {
            venue_instance_id: config.venue_instance_id.clone(),
            url: config
                .url
                .clone()
                .unwrap_or_else(|| "wss://api.hyperliquid.xyz/ws".to_string()),
            configured_catalog: config.catalog(),
            catalog_source: config.catalog_source,
            metadata_url: config.metadata_url.clone(),
            default_quote_asset: config.default_quote_asset.clone(),
            default_settle_asset: config.default_settle_asset.clone(),
            default_margin_asset: config.default_margin_asset.clone(),
            channel: config.channel.clone().unwrap_or_else(|| "bbo".to_string()),
        }
    }

    async fn run_once(
        &self,
        tx: Sender<MarketEvent>,
        mut shutdown: watch::Receiver<bool>,
    ) -> anyhow::Result<()> {
        let catalog = self.bootstrap_catalog().await?;
        for instrument in catalog.instruments() {
            tx.send(MarketEvent::Catalog {
                instrument: instrument.clone(),
            })
            .await
            .context("send Hyperliquid catalog")?;
        }

        let (stream, _) = ws::connect(&self.url).await?;
        let (mut write, mut read) = stream.split();

        for instrument in catalog.instruments() {
            let feed_key = instrument.feed_key();
            let payload = json!({
                "method": "subscribe",
                "subscription": {
                    "type": self.channel,
                    "coin": feed_key,
                }
            });
            write
                .send(Message::Text(payload.to_string()))
                .await
                .with_context(|| format!("subscribe Hyperliquid {feed_key}"))?;
        }

        info!(
            venue = %self.venue_instance_id,
            instruments = ?catalog.instruments(),
            "subscribed"
        );
        let mut heartbeat = time::interval(Duration::from_secs(30));

        loop {
            tokio::select! {
                changed = shutdown.changed() => {
                    if changed.is_ok() && *shutdown.borrow() {
                        debug!(venue = "hyperliquid", "shutdown received");
                        return Ok(());
                    }
                }
                _ = heartbeat.tick() => {
                    write.send(Message::Text(json!({"method":"ping"}).to_string())).await?;
                }
                maybe_msg = read.next() => {
                    let Some(msg) = maybe_msg else {
                        anyhow::bail!("Hyperliquid websocket closed");
                    };
                    match msg? {
                        Message::Text(text) => {
                            let recv_ts_ns = crate::ingest::time::unix_time_ns();
                            match parser::parse_message(&text, recv_ts_ns, &self.venue_instance_id) {
                                Ok(Some(tick)) => {
                                    match catalog.retarget_tick(tick) {
                                        Ok(tick) => tx.send(MarketEvent::Tick { tick }).await.context("send Hyperliquid tick")?,
                                        Err(miss) => warn_catalog_miss("hyperliquid", miss),
                                    }
                                }
                                Ok(None) => {}
                                Err(err) => {
                                    warn!(venue = "hyperliquid", error = %err, payload = %text, "failed to parse websocket message");
                                }
                            }
                        }
                        Message::Close(frame) => {
                            anyhow::bail!("Hyperliquid websocket closed: {frame:?}");
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

    async fn bootstrap_catalog(&self) -> anyhow::Result<CatalogIndex> {
        let configured = self.configured_catalog.clone();
        if self.catalog_source == CatalogSource::Exchange {
            match self.fetch_exchange_catalog().await {
                Ok(fetched) => {
                    let merged = merge_configured_catalog(configured, fetched);
                    info!(
                        venue = %self.venue_instance_id,
                        instruments = merged.len(),
                        "loaded Hyperliquid instrument catalog from exchange metadata"
                    );
                    return Ok(CatalogIndex::new(merged));
                }
                Err(err) => {
                    warn!(
                        venue = %self.venue_instance_id,
                        error = %err,
                        "failed to load Hyperliquid exchange catalog; falling back to configured instruments"
                    );
                }
            }
        }

        Ok(CatalogIndex::new(configured))
    }

    async fn fetch_exchange_catalog(&self) -> anyhow::Result<Vec<InstrumentCatalog>> {
        let url = self
            .metadata_url
            .as_deref()
            .unwrap_or("https://api.hyperliquid.xyz/info");
        let text = reqwest::Client::new()
            .post(url)
            .json(&json!({"type": "meta"}))
            .send()
            .await
            .with_context(|| format!("request Hyperliquid metadata {url}"))?
            .error_for_status()
            .with_context(|| format!("Hyperliquid metadata returned error status {url}"))?
            .text()
            .await
            .with_context(|| format!("read Hyperliquid metadata {url}"))?;
        parse_exchange_catalog(
            &text,
            &self.venue_instance_id,
            &self.default_quote_asset,
            &self.default_settle_asset,
            &self.default_margin_asset,
        )
    }
}

#[derive(Debug, Deserialize)]
struct HyperliquidAsset {
    name: String,
    #[serde(default, rename = "szDecimals")]
    sz_decimals: Option<u32>,
    #[serde(default, rename = "isDelisted")]
    is_delisted: Option<bool>,
}

fn parse_exchange_catalog(
    text: &str,
    venue_instance_id: &str,
    quote_asset: &str,
    settle_asset: &str,
    margin_asset: &str,
) -> anyhow::Result<Vec<InstrumentCatalog>> {
    let value: serde_json::Value = serde_json::from_str(text)?;
    let universe = value
        .get("universe")
        .and_then(serde_json::Value::as_array)
        .context("Hyperliquid metadata missing universe")?;

    universe
        .iter()
        .map(|raw| {
            let asset: HyperliquidAsset = serde_json::from_value(raw.clone())?;
            let status = if asset.is_delisted == Some(true) {
                "inactive"
            } else {
                "active"
            };
            Ok(InstrumentCatalog::new(
                venue_instance_id,
                asset.name.clone(),
                asset.name.clone(),
                Some(asset.name.clone()),
                ProductType::Perp,
                asset.name,
                quote_asset,
                settle_asset,
                margin_asset,
                None,
                asset.sz_decimals.and_then(decimal_tick),
                None,
                status,
                Some(raw.clone()),
            ))
        })
        .collect()
}

#[async_trait]
impl ExchangeAdapter for HyperliquidAdapter {
    async fn run(
        &self,
        tx: Sender<MarketEvent>,
        shutdown: watch::Receiver<bool>,
    ) -> anyhow::Result<()> {
        run_with_reconnect("hyperliquid", tx, shutdown, |tx, shutdown| {
            self.run_once(tx, shutdown)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::parse_exchange_catalog;

    #[test]
    fn parses_hyperliquid_exchange_catalog() {
        let raw = r#"{
            "universe": [
                {"name": "BTC", "szDecimals": 5, "maxLeverage": 40},
                {"name": "ETH", "szDecimals": 4, "isDelisted": true}
            ]
        }"#;

        let catalog = parse_exchange_catalog(raw, "hyperliquid", "USDC", "USDC", "USDC").unwrap();

        assert_eq!(catalog.len(), 2);
        assert_eq!(catalog[0].instrument_id, "BTC");
        assert_eq!(catalog[0].feed_key(), "BTC");
        assert_eq!(catalog[0].size_tick.unwrap().to_string(), "0.00001");
        assert_eq!(catalog[0].quote_asset, "USDC");
        assert_eq!(catalog[1].status, "inactive");
        assert!(catalog[0].source_raw_json.is_some());
    }
}
