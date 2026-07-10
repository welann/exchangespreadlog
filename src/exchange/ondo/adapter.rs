use std::{str::FromStr, time::Duration};

use anyhow::Context;
use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use reqwest::header::USER_AGENT;
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
        CatalogIndex, ExchangeAdapter, merge_configured_catalog, run_with_reconnect,
        warn_catalog_miss,
    },
    ingest::ws,
};

use super::parser::{self, ParsedMessage};

const DEFAULT_WS_URL: &str = "wss://api.ondoperps.xyz/ws";
const DEFAULT_METADATA_URL: &str = "https://api.ondoperps.xyz/v1/markets";
const DEFAULT_CHANNEL: &str = "topOfBooksPerps";
const USER_AGENT_VALUE: &str = "exchangespreadlog/0.1";

#[derive(Debug, Clone)]
pub struct OndoAdapter {
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

impl OndoAdapter {
    pub fn from_config(config: &VenueConfig) -> Self {
        Self {
            venue_instance_id: config.venue_instance_id.clone(),
            url: config
                .url
                .clone()
                .unwrap_or_else(|| DEFAULT_WS_URL.to_string()),
            channel: config
                .channel
                .clone()
                .unwrap_or_else(|| DEFAULT_CHANNEL.to_string()),
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
            .context("send Ondo catalog")?;
        }

        let (stream, _) = ws::connect(&self.url).await?;
        let (mut write, mut read) = stream.split();
        let payload = build_subscription_payload(&self.channel, catalog.instruments());
        write
            .send(Message::Text(payload))
            .await
            .context("subscribe Ondo top of book")?;

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
                        debug!(venue = "ondo", "shutdown received");
                        return Ok(());
                    }
                }
                _ = heartbeat.tick() => {
                    write.send(Message::Text(json!({"op": "ping"}).to_string())).await?;
                }
                maybe_msg = read.next() => {
                    let Some(msg) = maybe_msg else {
                        anyhow::bail!("Ondo websocket closed");
                    };
                    match msg? {
                        Message::Text(text) => {
                            let recv_ts_ns = crate::ingest::time::unix_time_ns();
                            match parser::parse_message(&text, recv_ts_ns, &self.venue_instance_id) {
                                Ok(ParsedMessage::Ticks(ticks)) => {
                                    for tick in ticks {
                                        match catalog.retarget_tick(tick) {
                                            Ok(tick) => tx.send(MarketEvent::Tick { tick }).await.context("send Ondo tick")?,
                                            Err(miss) => warn_catalog_miss("ondo", miss),
                                        }
                                    }
                                }
                                Ok(ParsedMessage::Ignore) => {}
                                Err(err) => {
                                    warn!(venue = "ondo", error = %err, payload = %text, "failed to parse websocket message");
                                }
                            }
                        }
                        Message::Close(frame) => {
                            anyhow::bail!("Ondo websocket closed: {frame:?}");
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
                        "loaded Ondo instrument catalog from exchange metadata"
                    );
                    return Ok(CatalogIndex::new(merged));
                }
                Err(err) => {
                    warn!(
                        venue = %self.venue_instance_id,
                        error = %err,
                        "failed to load Ondo exchange catalog; falling back to configured instruments"
                    );
                }
            }
        }

        Ok(CatalogIndex::new(configured))
    }

    async fn fetch_exchange_catalog(&self) -> anyhow::Result<Vec<InstrumentCatalog>> {
        let url = self.metadata_url.as_deref().unwrap_or(DEFAULT_METADATA_URL);
        let text = reqwest::Client::new()
            .get(url)
            .header(USER_AGENT, USER_AGENT_VALUE)
            .send()
            .await
            .with_context(|| format!("request Ondo markets {url}"))?
            .error_for_status()
            .with_context(|| format!("Ondo markets returned error status {url}"))?
            .text()
            .await
            .with_context(|| format!("read Ondo markets {url}"))?;

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
struct MarketsResponse {
    result: MarketsResult,
}

#[derive(Debug, Deserialize)]
struct MarketsResult {
    perps: PerpsMarkets,
}

#[derive(Debug, Deserialize)]
struct PerpsMarkets {
    #[serde(default, rename = "tradingPairs")]
    trading_pairs: Option<Vec<OndoMarket>>,
}

#[derive(Debug, Deserialize)]
struct OndoMarket {
    market: String,
    #[serde(default, rename = "displayName")]
    display_name: Option<String>,
    #[serde(default)]
    pair: Option<OndoPair>,
    #[serde(default, rename = "baseIncrement")]
    base_increment: Option<String>,
    #[serde(default, rename = "quoteIncrement")]
    quote_increment: Option<String>,
    #[serde(default)]
    disabled: bool,
}

#[derive(Debug, Deserialize)]
struct OndoPair {
    base: String,
    quote: String,
}

fn build_subscription_payload(channel: &str, instruments: &[InstrumentCatalog]) -> String {
    let markets = instruments
        .iter()
        .map(|instrument| instrument.feed_key().to_string())
        .collect::<Vec<_>>();
    json!({
        "op": "subscribe",
        "channel": channel,
        "markets": markets,
    })
    .to_string()
}

fn parse_exchange_catalog(
    text: &str,
    venue_instance_id: &str,
    quote_asset: &str,
    settle_asset: &str,
    margin_asset: &str,
) -> anyhow::Result<Vec<InstrumentCatalog>> {
    let response: MarketsResponse = serde_json::from_str(text)?;
    let raw_value: serde_json::Value = serde_json::from_str(text)?;
    let raw_markets = raw_value
        .pointer("/result/perps/tradingPairs")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();

    response
        .result
        .perps
        .trading_pairs
        .unwrap_or_default()
        .into_iter()
        .enumerate()
        .map(|(index, market)| {
            let pair = market.pair;
            let base_asset = pair
                .as_ref()
                .map(|pair| pair.base.clone())
                .unwrap_or_else(|| base_from_market(&market.market));
            let quote_asset = pair
                .as_ref()
                .map(|pair| pair.quote.clone())
                .unwrap_or_else(|| quote_asset.to_string());
            let raw_symbol = market
                .display_name
                .filter(|symbol| !symbol.is_empty())
                .unwrap_or_else(|| market.market.clone());
            let status = if market.disabled {
                "inactive"
            } else {
                "active"
            };

            Ok(InstrumentCatalog::new(
                venue_instance_id,
                market.market.clone(),
                raw_symbol,
                Some(market.market.clone()),
                ProductType::Perp,
                base_asset,
                quote_asset,
                settle_asset,
                margin_asset,
                parse_optional_fixed(market.quote_increment.as_deref())?,
                parse_optional_fixed(market.base_increment.as_deref())?,
                None,
                status,
                raw_markets.get(index).cloned(),
            ))
        })
        .collect()
}

fn parse_optional_fixed(value: Option<&str>) -> anyhow::Result<Option<Fixed>> {
    value
        .map(|value| Fixed::from_str(value).map_err(anyhow::Error::from))
        .transpose()
}

fn base_from_market(market: &str) -> String {
    market
        .split_once('-')
        .map(|(base, _)| base)
        .unwrap_or(market)
        .to_string()
}

#[async_trait]
impl ExchangeAdapter for OndoAdapter {
    async fn run(
        &self,
        tx: Sender<MarketEvent>,
        shutdown: watch::Receiver<bool>,
    ) -> anyhow::Result<()> {
        run_with_reconnect("ondo", tx, shutdown, |tx, shutdown| {
            self.run_once(tx, shutdown)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::{build_subscription_payload, parse_exchange_catalog};

    #[test]
    fn parses_ondo_exchange_catalog() {
        let raw = r#"{
            "success": true,
            "result": {
                "perps": {
                    "tradingPairs": [{
                        "market": "BTC-USD.P",
                        "displayName": "BTCUSD",
                        "pair": {"base": "BTC", "quote": "USD"},
                        "baseIncrement": "0.0001",
                        "quoteIncrement": "0.01"
                    }, {
                        "market": "SMSN-USD.P",
                        "displayName": "SMSNUSD",
                        "pair": {"base": "SMSN", "quote": "USD"},
                        "baseIncrement": "0.01",
                        "quoteIncrement": "0.01",
                        "disabled": true
                    }]
                }
            }
        }"#;

        let catalog = parse_exchange_catalog(raw, "ondo", "USD", "USD", "USD").unwrap();

        assert_eq!(catalog.len(), 2);
        assert_eq!(catalog[0].instrument_id, "BTC-USD.P");
        assert_eq!(catalog[0].raw_symbol, "BTCUSD");
        assert_eq!(catalog[0].feed_key(), "BTC-USD.P");
        assert_eq!(catalog[0].base_asset, "BTC");
        assert_eq!(catalog[0].quote_asset, "USD");
        assert_eq!(catalog[0].price_tick.unwrap().to_string(), "0.01");
        assert_eq!(catalog[0].size_tick.unwrap().to_string(), "0.0001");
        assert_eq!(catalog[1].status, "inactive");
        assert!(catalog[0].source_raw_json.is_some());
    }

    #[test]
    fn builds_subscription_payload_for_configured_markets() {
        let catalog = parse_exchange_catalog(
            r#"{
                "result": {
                    "perps": {
                        "tradingPairs": [{
                            "market": "BTC-USD.P",
                            "pair": {"base": "BTC", "quote": "USD"}
                        }, {
                            "market": "ETH-USD.P",
                            "pair": {"base": "ETH", "quote": "USD"}
                        }]
                    }
                }
            }"#,
            "ondo",
            "USD",
            "USD",
            "USD",
        )
        .unwrap();

        let payload = build_subscription_payload("topOfBooksPerps", &catalog);
        let value: serde_json::Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(value["op"], "subscribe");
        assert_eq!(value["channel"], "topOfBooksPerps");
        assert_eq!(value["markets"][0], "BTC-USD.P");
        assert_eq!(value["markets"][1], "ETH-USD.P");
    }
}
