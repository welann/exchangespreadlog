use std::{collections::HashMap, time::Duration};

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
    domain::{Fixed, InstrumentCatalog, InstrumentRef, MarketEvent, ProductType},
    exchange::{
        CatalogIndex, ExchangeAdapter, decimal_tick, merge_configured_catalog, run_with_reconnect,
        warn_catalog_miss,
    },
    ingest::ws,
};

use super::{
    orderbook::{ApplyResult, MarketScale, PerplBooks},
    parser::{self, ParsedMessage},
};

#[derive(Debug, Clone)]
pub struct PerplAdapter {
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

#[derive(Debug, Clone)]
struct SidTarget {
    instrument: InstrumentRef,
    scale: MarketScale,
}

impl PerplAdapter {
    pub fn from_config(config: &VenueConfig) -> Self {
        Self {
            venue_instance_id: config.venue_instance_id.clone(),
            url: config
                .url
                .clone()
                .unwrap_or_else(|| "wss://app.perpl.xyz/ws/v1/market-data".to_string()),
            channel: config
                .channel
                .clone()
                .unwrap_or_else(|| "order-book".to_string()),
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
            .context("send Perpl catalog")?;
        }

        let (stream, _) = ws::connect(&self.url).await?;
        let (mut write, mut read) = stream.split();
        let mut streams = HashMap::new();

        let subscriptions = catalog
            .instruments()
            .iter()
            .map(|instrument| {
                let feed_key = instrument.feed_key();
                let stream = format!("{}@{feed_key}", self.channel);
                streams.insert(stream.clone(), instrument.clone());
                json!({
                    "stream": stream,
                    "subscribe": true,
                })
            })
            .collect::<Vec<_>>();

        write
            .send(Message::Text(
                json!({
                    "mt": 5,
                    "subs": subscriptions,
                })
                .to_string(),
            ))
            .await
            .context("subscribe Perpl order books")?;

        info!(
            venue = %self.venue_instance_id,
            instruments = ?catalog.instruments(),
            "subscribed"
        );

        let mut sid_targets = HashMap::<u64, SidTarget>::new();
        let mut books = PerplBooks::default();
        let mut heartbeat = time::interval(Duration::from_secs(30));

        loop {
            tokio::select! {
                changed = shutdown.changed() => {
                    if changed.is_ok() && *shutdown.borrow() {
                        debug!(venue = "perpl", "shutdown received");
                        return Ok(());
                    }
                }
                _ = heartbeat.tick() => {
                    write.send(Message::Text(json!({"mt": 1, "t": crate::ingest::time::unix_time_ns() / 1_000_000}).to_string())).await?;
                }
                maybe_msg = read.next() => {
                    let Some(msg) = maybe_msg else {
                        anyhow::bail!("Perpl websocket closed");
                    };
                    match msg? {
                        Message::Text(text) => {
                            let recv_ts_ns = crate::ingest::time::unix_time_ns();
                            match parser::parse_message(&text) {
                                Ok(ParsedMessage::SubscriptionResponse(acks)) => {
                                    for ack in acks {
                                        if let Some(error) = ack.error {
                                            anyhow::bail!("Perpl subscription failed for {}: {error}", ack.stream);
                                        }
                                        let Some(instrument) = streams.get(&ack.stream) else {
                                            warn!(
                                                venue = %self.venue_instance_id,
                                                stream = %ack.stream,
                                                sid = ack.sid,
                                                "Perpl subscription ack did not match requested stream"
                                            );
                                            continue;
                                        };
                                        sid_targets.insert(
                                            ack.sid,
                                            SidTarget {
                                                instrument: instrument.instrument_ref(),
                                                scale: scale_from_catalog(instrument),
                                            },
                                        );
                                    }
                                }
                                Ok(ParsedMessage::L2Book(update)) => {
                                    let Some(target) = sid_targets.get(&update.sid).cloned() else {
                                        warn_catalog_miss(
                                            "perpl",
                                            crate::exchange::CatalogLookupMiss {
                                                venue_instance_id: self.venue_instance_id.clone(),
                                                feed_key: update.sid.to_string(),
                                            },
                                        );
                                        continue;
                                    };

                                    match books.apply(update, target.instrument, target.scale, recv_ts_ns) {
                                        ApplyResult::Tick(tick) => {
                                            tx.send(MarketEvent::Tick { tick }).await.context("send Perpl tick")?;
                                        }
                                        ApplyResult::Skipped => {}
                                    }
                                }
                                Ok(ParsedMessage::Ignore) => {}
                                Err(err) => {
                                    warn!(venue = "perpl", error = %err, payload = %text, "failed to parse websocket message");
                                }
                            }
                        }
                        Message::Close(frame) => {
                            anyhow::bail!("Perpl websocket closed: {frame:?}");
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
                        "loaded Perpl instrument catalog from exchange metadata"
                    );
                    return Ok(CatalogIndex::new(merged));
                }
                Err(err) => {
                    warn!(
                        venue = %self.venue_instance_id,
                        error = %err,
                        "failed to load Perpl exchange catalog; falling back to configured instruments"
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
            .unwrap_or("https://app.perpl.xyz/api/v1/pub/context");
        let text = reqwest::Client::new()
            .get(url)
            .send()
            .await
            .with_context(|| format!("request Perpl context {url}"))?
            .error_for_status()
            .with_context(|| format!("Perpl context returned error status {url}"))?
            .text()
            .await
            .with_context(|| format!("read Perpl context {url}"))?;

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
struct ContextResponse {
    markets: Vec<PerplMarket>,
}

#[derive(Debug, Deserialize)]
struct PerplMarket {
    id: u64,
    #[serde(default)]
    symbol: Option<String>,
    name: String,
    config: PerplMarketConfig,
    #[serde(default)]
    size_units: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PerplMarketConfig {
    #[serde(default)]
    is_open: bool,
    price_decimals: u32,
    size_decimals: u32,
}

fn parse_exchange_catalog(
    text: &str,
    venue_instance_id: &str,
    quote_asset: &str,
    settle_asset: &str,
    margin_asset: &str,
) -> anyhow::Result<Vec<InstrumentCatalog>> {
    let response: ContextResponse = serde_json::from_str(text)?;
    let raw_value: serde_json::Value = serde_json::from_str(text)?;
    let raw_markets = raw_value
        .get("markets")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();

    response
        .markets
        .into_iter()
        .enumerate()
        .map(|(index, market)| {
            let base_asset = market
                .symbol
                .as_deref()
                .filter(|symbol| !symbol.is_empty())
                .unwrap_or(&market.name)
                .to_string();
            let raw_symbol = market
                .size_units
                .as_deref()
                .filter(|symbol| !symbol.is_empty())
                .unwrap_or(&base_asset)
                .to_string();
            let instrument_id = market.id.to_string();
            let status = if market.config.is_open {
                "active"
            } else {
                "inactive"
            };

            Ok(InstrumentCatalog::new(
                venue_instance_id,
                instrument_id.clone(),
                raw_symbol,
                Some(instrument_id),
                ProductType::Perp,
                base_asset,
                quote_asset,
                settle_asset,
                margin_asset,
                decimal_tick(market.config.price_decimals),
                decimal_tick(market.config.size_decimals),
                None,
                status,
                raw_markets.get(index).cloned(),
            ))
        })
        .collect()
}

fn scale_from_catalog(instrument: &InstrumentCatalog) -> MarketScale {
    MarketScale {
        price_decimals: instrument.price_tick.map(Fixed::scale).unwrap_or_default(),
        size_decimals: instrument.size_tick.map(Fixed::scale).unwrap_or_default(),
    }
}

#[async_trait]
impl ExchangeAdapter for PerplAdapter {
    async fn run(
        &self,
        tx: Sender<MarketEvent>,
        shutdown: watch::Receiver<bool>,
    ) -> anyhow::Result<()> {
        run_with_reconnect("perpl", tx, shutdown, |tx, shutdown| {
            self.run_once(tx, shutdown)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::{parse_exchange_catalog, scale_from_catalog};

    #[test]
    fn parses_perpl_exchange_catalog() {
        let raw = r#"{
            "markets": [{
                "id": 1,
                "symbol": "",
                "name": "BTC",
                "size_units": "BTC",
                "config": {
                    "is_open": true,
                    "price_decimals": 1,
                    "size_decimals": 5
                }
            }, {
                "id": 20,
                "symbol": "ETH",
                "name": "ETH",
                "size_units": "ETH",
                "config": {
                    "is_open": false,
                    "price_decimals": 2,
                    "size_decimals": 3
                }
            }]
        }"#;

        let catalog = parse_exchange_catalog(raw, "perpl", "AUSD", "AUSD", "AUSD").unwrap();

        assert_eq!(catalog.len(), 2);
        assert_eq!(catalog[0].instrument_id, "1");
        assert_eq!(catalog[0].feed_key(), "1");
        assert_eq!(catalog[0].base_asset, "BTC");
        assert_eq!(catalog[0].quote_asset, "AUSD");
        assert_eq!(catalog[0].price_tick.unwrap().to_string(), "0.1");
        assert_eq!(catalog[0].size_tick.unwrap().to_string(), "0.00001");
        assert_eq!(catalog[1].status, "inactive");
        assert!(catalog[0].source_raw_json.is_some());

        let scale = scale_from_catalog(&catalog[0]);
        assert_eq!(scale.price_decimals, 1);
        assert_eq!(scale.size_decimals, 5);
    }
}
