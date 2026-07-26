use std::{collections::HashMap, str::FromStr, time::Duration};

use anyhow::{Context, bail};
use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::json;
use tokio::{
    sync::{mpsc::Sender, watch},
    time,
};
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, info};

use crate::{
    config::{CatalogSource, VenueConfig},
    domain::{Fixed, InstrumentCatalog, MarketEvent, ProductType},
    exchange::{CatalogIndex, ExchangeAdapter, run_with_reconnect, warn_catalog_miss},
    ingest::ws,
};

use super::{
    orderbook::{ApplyResult, EtherealBooks},
    parser::{self, ParsedMessage},
};

#[derive(Debug, Clone)]
pub struct EtherealAdapter {
    venue_instance_id: String,
    url: String,
    channel: String,
    configured_catalog: Vec<InstrumentCatalog>,
    catalog_source: CatalogSource,
    metadata_url: Option<String>,
    default_quote_asset: String,
    default_settle_asset: String,
    default_margin_asset: String,
}

impl EtherealAdapter {
    pub fn from_config(config: &VenueConfig) -> Self {
        Self {
            venue_instance_id: config.venue_instance_id.clone(),
            url: config.url.clone().unwrap_or_else(|| {
                "wss://ws.ethereal.trade/socket.io/?EIO=4&transport=websocket".to_string()
            }),
            channel: config
                .channel
                .clone()
                .unwrap_or_else(|| "BookDepth".to_string()),
            configured_catalog: config.catalog(),
            catalog_source: config.catalog_source,
            metadata_url: config.metadata_url.clone(),
            default_quote_asset: config.default_quote_asset.clone(),
            default_settle_asset: config.default_settle_asset.clone(),
            default_margin_asset: config.default_margin_asset.clone(),
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
            .context("send Ethereal catalog")?;
        }

        let (stream, _) = ws::connect(&self.url).await?;
        let (mut write, mut read) = stream.split();

        time::timeout(Duration::from_secs(10), async {
            loop {
                let message = read
                    .next()
                    .await
                    .context("Ethereal websocket closed during Socket.IO handshake")??;
                match message {
                    Message::Text(text) => match parser::parse_message(&text)? {
                        ParsedMessage::EngineOpen => {
                            write
                                .send(Message::Text("40/v1/stream,".to_string()))
                                .await
                                .context("connect Ethereal Socket.IO namespace")?;
                        }
                        ParsedMessage::NamespaceConnected => return Ok::<_, anyhow::Error>(()),
                        ParsedMessage::EnginePing => {
                            write.send(Message::Text("3".to_string())).await?;
                        }
                        ParsedMessage::L2Book(_) | ParsedMessage::Ignore => {}
                    },
                    Message::Ping(payload) => write.send(Message::Pong(payload)).await?,
                    Message::Close(frame) => {
                        bail!("Ethereal websocket closed during handshake: {frame:?}")
                    }
                    _ => {}
                }
            }
        })
        .await
        .context("Ethereal Socket.IO namespace handshake timed out")??;

        for instrument in catalog.instruments() {
            let feed_key = instrument.feed_key();
            let payload = build_subscription_payload(&self.channel, feed_key);
            write
                .send(Message::Text(payload))
                .await
                .with_context(|| format!("subscribe Ethereal {feed_key}"))?;
        }

        info!(
            venue = %self.venue_instance_id,
            instruments = catalog.instruments().len(),
            "Socket.IO subscription requests sent"
        );
        let mut books = EtherealBooks::default();

        loop {
            tokio::select! {
                changed = shutdown.changed() => {
                    if changed.is_ok() && *shutdown.borrow() {
                        debug!(venue = "ethereal", "shutdown received");
                        return Ok(());
                    }
                }
                maybe_msg = read.next() => {
                    let Some(msg) = maybe_msg else {
                        anyhow::bail!("Ethereal websocket closed");
                    };
                    match msg? {
                        Message::Text(text) => {
                            let recv_ts_ns = crate::ingest::time::unix_time_ns();
                            match parser::parse_message(&text) {
                                Ok(ParsedMessage::L2Book(update)) => {
                                    let symbol = update.symbol.clone();
                                    let Some(instrument) = catalog.resolve(&symbol) else {
                                        warn_catalog_miss(
                                            "ethereal",
                                            crate::exchange::CatalogLookupMiss {
                                                venue_instance_id: self.venue_instance_id.clone(),
                                                feed_key: symbol,
                                            },
                                        );
                                        continue;
                                    };

                                    match books.apply(update, instrument, recv_ts_ns) {
                                        ApplyResult::Tick(tick) => {
                                            tx.send(MarketEvent::Tick { tick }).await.context("send Ethereal tick")?;
                                        }
                                        ApplyResult::Skipped => {}
                                        ApplyResult::Gap {
                                            symbol,
                                            expected_previous_ts_ms,
                                            received_previous_ts_ms,
                                            received_ts_ms,
                                        } => {
                                            anyhow::bail!(
                                                "Ethereal L2Book gap for {symbol}: expected pt={expected_previous_ts_ms}, received pt={received_previous_ts_ms:?}, t={received_ts_ms}"
                                            );
                                        }
                                    }
                                }
                                Ok(ParsedMessage::EnginePing) => {
                                    write.send(Message::Text("3".to_string())).await?;
                                }
                                Ok(ParsedMessage::EngineOpen | ParsedMessage::NamespaceConnected) => {}
                                Ok(ParsedMessage::Ignore) => {}
                                Err(err) => {
                                    anyhow::bail!("Ethereal protocol error: {err}; payload={text}");
                                }
                            }
                        }
                        Message::Close(frame) => {
                            anyhow::bail!("Ethereal websocket closed: {frame:?}");
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
                    let merged = merge_ethereal_catalog(configured, fetched);
                    if merged.is_empty() {
                        bail!("Ethereal metadata contains none of the configured products");
                    }
                    info!(
                        venue = %self.venue_instance_id,
                        instruments = merged.len(),
                        "loaded Ethereal instrument catalog from exchange metadata"
                    );
                    return Ok(CatalogIndex::new(merged));
                }
                Err(err) => {
                    return Err(err).context(
                        "Ethereal metadata is required because mainnet subscriptions use product UUIDs",
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
            .unwrap_or("https://api.ethereal.trade/v1/product");
        let text = reqwest::Client::new()
            .get(url)
            .send()
            .await
            .with_context(|| format!("request Ethereal metadata {url}"))?
            .error_for_status()
            .with_context(|| format!("Ethereal metadata returned error status {url}"))?
            .text()
            .await
            .with_context(|| format!("read Ethereal metadata {url}"))?;

        parse_exchange_catalog(
            &text,
            &self.venue_instance_id,
            &self.default_quote_asset,
            &self.default_settle_asset,
            &self.default_margin_asset,
        )
    }
}

#[async_trait]
impl ExchangeAdapter for EtherealAdapter {
    async fn run(
        &self,
        tx: Sender<MarketEvent>,
        shutdown: watch::Receiver<bool>,
    ) -> anyhow::Result<()> {
        run_with_reconnect("ethereal", tx, shutdown, |tx, shutdown| {
            self.run_once(tx, shutdown)
        })
        .await
    }
}

fn build_subscription_payload(channel: &str, symbol: &str) -> String {
    format!(
        "42/v1/stream,{}",
        json!([
            "subscribe",
            {
            "type": channel,
            "productId": symbol,
        }
        ])
    )
}

#[derive(Debug, Deserialize)]
struct ProductResponse {
    data: Vec<Product>,
}

#[derive(Debug, Deserialize)]
struct Product {
    id: String,
    ticker: String,
    #[serde(rename = "displayTicker")]
    display_ticker: String,
    #[serde(default, rename = "baseTokenName")]
    base_token_name: Option<String>,
    #[serde(default, rename = "quoteTokenName")]
    quote_token_name: Option<String>,
    #[serde(rename = "tickSize")]
    tick_size: String,
    #[serde(rename = "lotSize")]
    lot_size: String,
    #[serde(default, rename = "minQuantity")]
    min_quantity: Option<String>,
    status: String,
}

fn parse_exchange_catalog(
    text: &str,
    venue_instance_id: &str,
    default_quote_asset: &str,
    default_settle_asset: &str,
    default_margin_asset: &str,
) -> anyhow::Result<Vec<InstrumentCatalog>> {
    let response: ProductResponse = serde_json::from_str(text)?;
    let raw_value: serde_json::Value = serde_json::from_str(text)?;
    let raw_products = raw_value
        .get("data")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();

    response
        .data
        .into_iter()
        .enumerate()
        .map(|(index, product)| {
            let quote_asset = product
                .quote_token_name
                .clone()
                .unwrap_or_else(|| default_quote_asset.to_string());
            let base_asset = product
                .base_token_name
                .clone()
                .unwrap_or_else(|| base_from_ticker(&product.ticker, &quote_asset));
            let status = if product.status.eq_ignore_ascii_case("ACTIVE") {
                "active"
            } else {
                "inactive"
            };

            Ok(InstrumentCatalog::new(
                venue_instance_id,
                product.ticker.clone(),
                product.display_ticker,
                Some(product.id),
                ProductType::Perp,
                base_asset,
                quote_asset,
                default_settle_asset,
                default_margin_asset,
                Some(Fixed::from_str(&product.tick_size)?),
                Some(Fixed::from_str(&product.lot_size)?),
                product
                    .min_quantity
                    .as_deref()
                    .map(Fixed::from_str)
                    .transpose()?,
                status,
                raw_products.get(index).cloned(),
            ))
        })
        .collect()
}

fn merge_ethereal_catalog(
    configured: Vec<InstrumentCatalog>,
    fetched: Vec<InstrumentCatalog>,
) -> Vec<InstrumentCatalog> {
    if configured.is_empty() {
        return fetched;
    }
    let fetched_by_instrument = fetched
        .into_iter()
        .map(|instrument| (instrument.instrument_id.clone(), instrument))
        .collect::<HashMap<_, _>>();
    configured
        .into_iter()
        .filter_map(|configured| {
            let fetched = fetched_by_instrument.get(&configured.instrument_id)?;
            Some(InstrumentCatalog::new_with_units(
                configured.venue_instance_id,
                configured.instrument_id,
                configured.raw_symbol,
                fetched.feed_symbol.clone(),
                configured.product_type,
                configured.base_asset,
                fetched.quote_asset.clone(),
                fetched.settle_asset.clone(),
                fetched.margin_asset.clone(),
                configured.price_convention,
                configured.size_unit,
                fetched.price_tick.or(configured.price_tick),
                fetched.size_tick.or(configured.size_tick),
                fetched.min_size.or(configured.min_size),
                fetched.status.clone(),
                fetched.source_raw_json.clone(),
            ))
        })
        .collect()
}

fn base_from_ticker(ticker: &str, quote_asset: &str) -> String {
    ticker
        .strip_suffix(quote_asset)
        .filter(|base| !base.is_empty())
        .unwrap_or(ticker)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::{base_from_ticker, build_subscription_payload, parse_exchange_catalog};
    #[test]
    fn builds_l2_book_subscription_payload() {
        let payload = build_subscription_payload("BookDepth", "product-1");

        assert_eq!(
            payload,
            r#"42/v1/stream,["subscribe",{"productId":"product-1","type":"BookDepth"}]"#
        );
    }

    #[test]
    fn parses_product_catalog() {
        let raw = r#"{
            "data": [{
                "id": "bc7d5575-3711-4532-a000-312bfacfb767",
                "ticker": "BTCUSD",
                "displayTicker": "BTC-USD",
                "baseTokenName": "BTC",
                "quoteTokenName": "USD",
                "tickSize": "1",
                "lotSize": "0.00001",
                "minQuantity": "0.00015",
                "status": "ACTIVE"
            }]
        }"#;

        let catalog = parse_exchange_catalog(raw, "ethereal", "USD", "USD", "USD").unwrap();
        assert_eq!(catalog.len(), 1);
        assert_eq!(catalog[0].instrument_id, "BTCUSD");
        assert_eq!(
            catalog[0].feed_key(),
            "bc7d5575-3711-4532-a000-312bfacfb767"
        );
        assert_eq!(catalog[0].raw_symbol, "BTC-USD");
        assert_eq!(catalog[0].base_asset, "BTC");
        assert_eq!(catalog[0].price_tick.unwrap().to_string(), "1");
        assert_eq!(catalog[0].size_tick.unwrap().to_string(), "0.00001");
        assert_eq!(catalog[0].min_size.unwrap().to_string(), "0.00015");
        assert_eq!(catalog[0].status, "active");
    }

    #[test]
    fn derives_base_from_ticker_when_metadata_omits_token_name() {
        assert_eq!(base_from_ticker("HYPEUSD", "USD"), "HYPE");
        assert_eq!(base_from_ticker("BTC-PERP", "USD"), "BTC-PERP");
    }
}
