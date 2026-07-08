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
    domain::{Fixed, InstrumentCatalog, MarketEvent, ProductType},
    exchange::{
        CatalogIndex, ExchangeAdapter, decimal_tick, merge_configured_catalog,
        run_with_reconnect_backoff, warn_catalog_miss,
    },
    ingest::{supervisor::Backoff, ws},
};

use super::parser;

#[derive(Debug, Clone)]
pub struct LighterAdapter {
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

impl LighterAdapter {
    pub fn from_config(config: &VenueConfig) -> Self {
        Self {
            venue_instance_id: config.venue_instance_id.clone(),
            url: config.url.clone().unwrap_or_else(|| {
                "wss://mainnet.zklighter.elliot.ai/stream?readonly=true".to_string()
            }),
            configured_catalog: config.catalog(),
            catalog_source: config.catalog_source,
            metadata_url: config.metadata_url.clone(),
            default_quote_asset: config.default_quote_asset.clone(),
            default_settle_asset: config.default_settle_asset.clone(),
            default_margin_asset: config.default_margin_asset.clone(),
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
        let catalog = self.bootstrap_catalog().await?;
        for instrument in catalog.instruments() {
            tx.send(MarketEvent::Catalog {
                instrument: instrument.clone(),
            })
            .await
            .context("send Lighter catalog")?;
        }

        let (stream, _) = ws::connect(&self.url).await?;
        let (mut write, mut read) = stream.split();

        for instrument in catalog.instruments() {
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

        info!(venue = %self.venue_instance_id, instruments = ?catalog.instruments(), "subscribed");
        let mut heartbeat = time::interval(Duration::from_secs(15));

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
                                    match catalog.retarget_tick(tick) {
                                        Ok(tick) => tx.send(MarketEvent::Tick { tick }).await.context("send Lighter tick")?,
                                        Err(miss) => warn_catalog_miss("lighter", miss),
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

    async fn bootstrap_catalog(&self) -> anyhow::Result<CatalogIndex> {
        let configured = self.configured_catalog.clone();
        if self.catalog_source == CatalogSource::Exchange {
            match self.fetch_exchange_catalog().await {
                Ok(fetched) => {
                    let merged = merge_configured_catalog(configured, fetched);
                    info!(
                        venue = %self.venue_instance_id,
                        instruments = merged.len(),
                        "loaded Lighter instrument catalog from exchange metadata"
                    );
                    return Ok(CatalogIndex::new(merged));
                }
                Err(err) => {
                    warn!(
                        venue = %self.venue_instance_id,
                        error = %err,
                        "failed to load Lighter exchange catalog; falling back to configured instruments"
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
            .unwrap_or("https://mainnet.zklighter.elliot.ai/api/v1/orderBooks");
        let text = reqwest::Client::new()
            .get(url)
            .send()
            .await
            .with_context(|| format!("request Lighter order book metadata {url}"))?
            .error_for_status()
            .with_context(|| format!("Lighter metadata returned error status {url}"))?
            .text()
            .await
            .with_context(|| format!("read Lighter metadata {url}"))?;
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
struct LighterOrderBook {
    symbol: String,
    market_id: u64,
    market_type: String,
    status: String,
    #[serde(default)]
    min_base_amount: Option<String>,
    #[serde(default)]
    supported_size_decimals: Option<u32>,
    #[serde(default)]
    supported_price_decimals: Option<u32>,
}

fn parse_exchange_catalog(
    text: &str,
    venue_instance_id: &str,
    default_quote_asset: &str,
    default_settle_asset: &str,
    default_margin_asset: &str,
) -> anyhow::Result<Vec<InstrumentCatalog>> {
    let value: serde_json::Value = serde_json::from_str(text)?;
    let order_books = value
        .get("order_books")
        .and_then(serde_json::Value::as_array)
        .context("Lighter metadata missing order_books")?;

    order_books
        .iter()
        .map(|raw| {
            let order_book: LighterOrderBook = serde_json::from_value(raw.clone())?;
            let (base_asset, quote_asset) =
                split_lighter_symbol(&order_book.symbol, default_quote_asset);
            Ok(InstrumentCatalog::new(
                venue_instance_id,
                order_book.market_id.to_string(),
                order_book.symbol.clone(),
                Some(order_book.market_id.to_string()),
                lighter_product_type(&order_book.market_type),
                base_asset,
                quote_asset,
                default_settle_asset,
                default_margin_asset,
                order_book.supported_price_decimals.and_then(decimal_tick),
                order_book.supported_size_decimals.and_then(decimal_tick),
                order_book
                    .min_base_amount
                    .as_deref()
                    .map(str::parse::<Fixed>)
                    .transpose()?,
                order_book.status,
                Some(raw.clone()),
            ))
        })
        .collect()
}

fn split_lighter_symbol(symbol: &str, default_quote_asset: &str) -> (String, String) {
    symbol
        .split_once('/')
        .map(|(base, quote)| (base.to_string(), quote.to_string()))
        .unwrap_or_else(|| (symbol.to_string(), default_quote_asset.to_string()))
}

fn lighter_product_type(value: &str) -> ProductType {
    match value {
        "spot" => ProductType::Spot,
        "future" => ProductType::Future,
        _ => ProductType::Perp,
    }
}

#[async_trait]
impl ExchangeAdapter for LighterAdapter {
    async fn run(
        &self,
        tx: Sender<MarketEvent>,
        shutdown: watch::Receiver<bool>,
    ) -> anyhow::Result<()> {
        let backoff = Backoff::new(Duration::from_secs(15), Duration::from_secs(300));
        run_with_reconnect_backoff("lighter", tx, shutdown, backoff, |tx, shutdown| {
            self.run_once(tx, shutdown)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::parse_exchange_catalog;
    use crate::domain::ProductType;

    #[test]
    fn parses_lighter_exchange_catalog() {
        let raw = r#"{
            "code": 200,
            "order_books": [
                {
                    "symbol": "BTC",
                    "market_id": 1,
                    "market_type": "perp",
                    "status": "active",
                    "min_base_amount": "0.0001",
                    "supported_size_decimals": 4,
                    "supported_price_decimals": 1
                },
                {
                    "symbol": "UNI/USDC",
                    "market_id": 2051,
                    "market_type": "spot",
                    "status": "active",
                    "min_base_amount": "1.00",
                    "supported_size_decimals": 2,
                    "supported_price_decimals": 4
                }
            ]
        }"#;

        let catalog = parse_exchange_catalog(raw, "lighter", "USDC", "USDC", "USDC").unwrap();

        assert_eq!(catalog.len(), 2);
        assert_eq!(catalog[0].instrument_id, "1");
        assert_eq!(catalog[0].raw_symbol, "BTC");
        assert_eq!(catalog[0].feed_key(), "1");
        assert_eq!(catalog[0].base_asset, "BTC");
        assert_eq!(catalog[0].price_tick.unwrap().to_string(), "0.1");
        assert_eq!(catalog[0].size_tick.unwrap().to_string(), "0.0001");
        assert_eq!(catalog[0].min_size.unwrap().to_string(), "0.0001");
        assert_eq!(catalog[1].product_type, ProductType::Spot);
        assert_eq!(catalog[1].base_asset, "UNI");
        assert_eq!(catalog[1].quote_asset, "USDC");
        assert!(catalog[0].source_raw_json.is_some());
    }
}
