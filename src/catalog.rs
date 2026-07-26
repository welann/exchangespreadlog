use std::{
    collections::{HashMap, HashSet},
    path::Path,
};

use anyhow::{Context, bail};
use futures_util::{
    FutureExt,
    future::{BoxFuture, join_all},
};
use reqwest::Client;
use serde_json::{Value, json};
use tracing::{info, warn};

use crate::{
    config::{CatalogSource, InstrumentConfig, VenueConfig},
    domain::{Fixed, PriceConvention, ProductType, SizeUnit},
};

const HYPERLIQUID_INFO_URL: &str = "https://api.hyperliquid.xyz/info";
const LIGHTER_ORDERBOOKS_URL: &str = "https://mainnet.zklighter.elliot.ai/api/v1/orderBooks";
const RISEX_MARKETS_URL: &str = "https://api.rise.trade/v1/markets";
const ZERO_ONE_INFO_URL: &str = "https://zo-mainnet.n1.xyz/info";
// Ethereal is intentionally disabled.
// const ETHEREAL_PRODUCTS_URL: &str = "https://api.ethereal.trade/v1/product";
const PERPL_CONTEXT_URL: &str = "https://app.perpl.xyz/api/v1/pub/context";
const ONDO_MARKETS_URL: &str = "https://api.ondoperps.xyz/v1/markets";
const SUPPORTED_QUOTE_ASSETS: &[&str] = &["USD", "USDC", "USDT", "AUSD"];

pub async fn discover_venue_configs(client: &Client) -> anyhow::Result<Vec<VenueConfig>> {
    discover_venue_configs_with_fallback(client, &[], &HashSet::new()).await
}

pub async fn discover_venue_configs_with_fallback(
    client: &Client,
    fallback: &[VenueConfig],
    disabled_venues: &HashSet<String>,
) -> anyhow::Result<Vec<VenueConfig>> {
    let jobs: Vec<(&str, BoxFuture<'_, anyhow::Result<VenueConfig>>)> = vec![
        ("hyperliquid", discover_hyperliquid(client).boxed()),
        ("lighter", discover_lighter(client).boxed()),
        ("risex", discover_risex(client).boxed()),
        ("01", discover_zero_one(client).boxed()),
        // Ethereal discovery is intentionally disabled.
        // ("ethereal", discover_ethereal(client).boxed()),
        ("perpl", discover_perpl(client).boxed()),
        ("ondo", discover_ondo(client).boxed()),
    ]
    .into_iter()
    .filter(|(venue, _)| !disabled_venues.contains(*venue))
    .collect();

    let results = join_all(
        jobs.into_iter()
            .map(|(venue, future)| async move { (venue, future.await) }),
    )
    .await;

    let mut venues = Vec::new();
    let fallback = fallback
        .iter()
        .map(|venue| (venue.venue_instance_id.as_str(), venue))
        .collect::<HashMap<_, _>>();
    for (venue, result) in results {
        match result {
            Ok(config) => {
                info!(
                    venue,
                    instruments = config.instruments.len(),
                    "discovered live exchange catalog"
                );
                venues.push(config);
            }
            Err(error) => {
                if let Some(cached) = fallback.get(venue) {
                    warn!(
                        venue,
                        %error,
                        instruments = cached.instruments.len(),
                        "exchange catalog discovery failed; retaining last-known-good catalog"
                    );
                    venues.push((*cached).clone());
                } else {
                    warn!(
                        venue,
                        %error,
                        "exchange catalog discovery failed; venue will be unavailable"
                    );
                }
            }
        }
    }

    let selected_markets = select_lighter_anchored_catalog(&mut venues)?;

    if venues.len() < 2 {
        bail!("live catalogs have no cross-venue comparable base assets");
    }

    info!(
        venues = venues.len(),
        markets = selected_markets,
        instruments = venues
            .iter()
            .map(|venue| venue.instruments.len())
            .sum::<usize>(),
        "selected cross-venue catalog"
    );
    Ok(venues)
}

pub async fn load_catalog_cache(path: &Path) -> anyhow::Result<Vec<VenueConfig>> {
    let text = tokio::fs::read_to_string(path)
        .await
        .with_context(|| format!("read catalog cache {}", path.display()))?;
    let venues = serde_json::from_str(&text)
        .with_context(|| format!("decode catalog cache {}", path.display()))?;
    Ok(venues)
}

pub async fn save_catalog_cache(path: &Path, venues: &[VenueConfig]) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .with_context(|| format!("create catalog cache directory {}", parent.display()))?;
    }
    let temporary = path.with_extension("json.tmp");
    let payload = serde_json::to_vec_pretty(venues)?;
    tokio::fs::write(&temporary, payload)
        .await
        .with_context(|| format!("write catalog cache {}", temporary.display()))?;
    tokio::fs::rename(&temporary, path)
        .await
        .with_context(|| format!("replace catalog cache {}", path.display()))?;
    Ok(())
}

fn select_lighter_anchored_catalog(venues: &mut Vec<VenueConfig>) -> anyhow::Result<usize> {
    let lighter_bases = venues
        .iter()
        .find(|venue| venue.venue_instance_id == "lighter")
        .context("Lighter catalog is required as the subscription anchor")?
        .instruments
        .iter()
        .map(|instrument| instrument.base_asset.clone())
        .collect::<Vec<_>>();
    let other_bases = venues
        .iter()
        .filter(|venue| venue.venue_instance_id != "lighter")
        .flat_map(|venue| {
            venue
                .instruments
                .iter()
                .map(|instrument| instrument.base_asset.clone())
        })
        .collect::<HashSet<_>>();
    let selected_bases = lighter_bases
        .into_iter()
        .filter(|base| other_bases.contains(base))
        .collect::<Vec<_>>();
    let selected_set = selected_bases.iter().cloned().collect::<HashSet<_>>();
    let selected_rank = selected_bases
        .iter()
        .enumerate()
        .map(|(index, base)| (base.clone(), index))
        .collect::<HashMap<_, _>>();

    for venue in venues.iter_mut() {
        let discovered = venue.instruments.len();
        venue.instruments = dedupe_instruments_by_base(std::mem::take(&mut venue.instruments));
        venue
            .instruments
            .retain(|instrument| selected_set.contains(&instrument.base_asset));
        venue.instruments.sort_by_key(|instrument| {
            selected_rank
                .get(&instrument.base_asset)
                .copied()
                .unwrap_or(usize::MAX)
        });
        info!(
            venue = %venue.venue_instance_id,
            discovered,
            selected = venue.instruments.len(),
            excluded = discovered - venue.instruments.len(),
            "built Lighter-anchored subscription catalog"
        );
    }
    venues.retain(|venue| !venue.instruments.is_empty());
    Ok(selected_bases.len())
}

async fn discover_hyperliquid(client: &Client) -> anyhow::Result<VenueConfig> {
    let (all_perp_metas, spot_meta) = tokio::try_join!(
        post_json(
            client,
            HYPERLIQUID_INFO_URL,
            json!({"type": "allPerpMetas"}),
        ),
        post_json(client, HYPERLIQUID_INFO_URL, json!({"type": "spotMeta"}),),
    )?;
    let instruments = parse_hyperliquid_instruments(&all_perp_metas, &spot_meta)?;

    Ok(venue(
        "hyperliquid",
        "hyperliquid",
        "wss://api.hyperliquid.xyz/ws",
        Some("bbo"),
        "USDC",
        Some(HYPERLIQUID_INFO_URL),
        instruments,
    ))
}

fn parse_hyperliquid_instruments(
    all_perp_metas: &Value,
    spot_meta: &Value,
) -> anyhow::Result<Vec<InstrumentConfig>> {
    let tokens = array_at(spot_meta, &["tokens"], "Hyperliquid spotMeta.tokens")?;
    let quote_by_index = tokens
        .iter()
        .filter_map(|token| {
            Some((
                token.get("index")?.as_u64()?,
                string_at(token, "name")?.to_ascii_uppercase(),
            ))
        })
        .collect::<HashMap<_, _>>();
    let metas = match all_perp_metas {
        Value::Array(values) => values.iter().collect::<Vec<_>>(),
        Value::Object(_) => vec![all_perp_metas],
        _ => bail!("Hyperliquid allPerpMetas returned an unsupported payload"),
    };

    let mut instruments = Vec::new();
    for meta in metas {
        let collateral_token = meta
            .get("collateralToken")
            .and_then(Value::as_u64)
            .context("Hyperliquid perp metadata is missing collateralToken")?;
        let quote_asset = quote_by_index.get(&collateral_token).with_context(|| {
            format!("Hyperliquid collateralToken {collateral_token} is absent from spotMeta")
        })?;
        if !SUPPORTED_QUOTE_ASSETS.contains(&quote_asset.as_str()) {
            warn!(
                collateral_token,
                quote_asset,
                "excluding Hyperliquid perp dex because its quote asset has no configured conversion"
            );
            continue;
        }
        let universe = meta
            .get("universe")
            .and_then(Value::as_array)
            .context("Hyperliquid perp metadata is missing universe")?;
        instruments.extend(universe.iter().filter_map(|market| {
            if market.get("isDelisted").and_then(Value::as_bool) == Some(true) {
                return None;
            }
            let symbol = string_at(market, "name")?;
            let size_decimals = market.get("szDecimals").and_then(Value::as_u64);
            Some(instrument(
                symbol,
                symbol,
                symbol,
                &normalize_base(symbol),
                quote_asset,
                None,
                size_decimals.and_then(decimal_tick),
            ))
        }));
    }
    if instruments.is_empty() {
        bail!("Hyperliquid has no active perps with a supported quote asset");
    }
    Ok(instruments)
}

async fn discover_lighter(client: &Client) -> anyhow::Result<VenueConfig> {
    let value = get_json(client, LIGHTER_ORDERBOOKS_URL).await?;
    let books = array_at(&value, &["order_books"], "Lighter order_books")?;
    let instruments = books
        .iter()
        .filter(|book| string_at(book, "market_type") == Some("perp"))
        .filter(|book| string_at(book, "status") == Some("active"))
        .filter_map(|book| {
            let market_id = scalar_string(book.get("market_id")?)?;
            let symbol = string_at(book, "symbol")?;
            let price_decimals = book.get("supported_price_decimals").and_then(Value::as_u64);
            let size_decimals = book.get("supported_size_decimals").and_then(Value::as_u64);
            let min_size = book
                .get("min_base_amount")
                .and_then(Value::as_str)
                .and_then(|value| value.parse().ok());
            Some(InstrumentConfig {
                min_size,
                ..instrument(
                    &market_id,
                    symbol,
                    &market_id,
                    &normalize_base(symbol),
                    "USDC",
                    price_decimals.and_then(decimal_tick),
                    size_decimals.and_then(decimal_tick),
                )
            })
        })
        .collect();

    Ok(venue(
        "lighter",
        "lighter",
        "wss://mainnet.zklighter.elliot.ai/stream?readonly=true",
        Some("ticker"),
        "USDC",
        Some(LIGHTER_ORDERBOOKS_URL),
        instruments,
    ))
}

async fn discover_risex(client: &Client) -> anyhow::Result<VenueConfig> {
    let value = get_json(client, RISEX_MARKETS_URL).await?;
    let markets = array_at(&value, &["data", "markets"], "RiseX data.markets")?;
    let instruments = markets
        .iter()
        .filter(|market| market.get("active").and_then(Value::as_bool) != Some(false))
        .filter_map(|market| {
            let market_id = scalar_string(market.get("market_id")?)?;
            let config = market.get("config")?.as_object()?;
            if config.get("unlocked").and_then(Value::as_bool) == Some(false) {
                return None;
            }
            let raw = config
                .get("name")
                .and_then(Value::as_str)
                .or_else(|| market.get("display_name").and_then(Value::as_str))?;
            let quote = market
                .get("quote_asset_symbol")
                .and_then(Value::as_str)
                .unwrap_or("USDC");
            Some(instrument(
                &market_id,
                raw,
                &market_id,
                &normalize_base(raw),
                quote,
                config
                    .get("step_price")
                    .and_then(Value::as_str)
                    .and_then(|value| value.parse().ok()),
                config
                    .get("step_size")
                    .and_then(Value::as_str)
                    .and_then(|value| value.parse().ok()),
            ))
        })
        .collect();

    Ok(venue(
        "risex",
        "risex",
        "wss://ws.rise.trade/ws",
        Some("orderbook"),
        "USDC",
        Some(RISEX_MARKETS_URL),
        instruments,
    ))
}

async fn discover_zero_one(client: &Client) -> anyhow::Result<VenueConfig> {
    let value = get_json(client, ZERO_ONE_INFO_URL).await?;
    let markets = array_at(&value, &["markets"], "01 markets")?;
    let instruments = markets
        .iter()
        .filter_map(|market| {
            let market_id = scalar_string(market.get("marketId")?)?;
            let symbol = string_at(market, "symbol")?;
            let base = normalize_base(symbol);
            if base == symbol {
                return None;
            }
            Some(instrument(
                &market_id,
                symbol,
                symbol,
                &base,
                "USD",
                market
                    .get("priceDecimals")
                    .and_then(Value::as_u64)
                    .and_then(decimal_tick),
                market
                    .get("sizeDecimals")
                    .and_then(Value::as_u64)
                    .and_then(decimal_tick),
            ))
        })
        .collect();

    Ok(venue(
        "01",
        "01",
        "wss://zo-mainnet.n1.xyz",
        Some("deltas"),
        "USD",
        Some(ZERO_ONE_INFO_URL),
        instruments,
    ))
}

// Ethereal catalog discovery is intentionally disabled together with its adapter.
async fn discover_perpl(client: &Client) -> anyhow::Result<VenueConfig> {
    let value = get_json(client, PERPL_CONTEXT_URL).await?;
    let markets = array_at(&value, &["markets"], "Perpl markets")?;
    let instruments = markets
        .iter()
        .filter(|market| {
            market
                .get("config")
                .and_then(|config| config.get("is_open"))
                .and_then(Value::as_bool)
                == Some(true)
        })
        .filter_map(|market| {
            let market_id = scalar_string(market.get("id")?)?;
            let raw = string_at(market, "size_units").or_else(|| string_at(market, "name"))?;
            let base = normalize_base(
                string_at(market, "symbol")
                    .filter(|value| !value.is_empty())
                    .or_else(|| string_at(market, "name"))
                    .unwrap_or(raw),
            );
            let config = market.get("config")?;
            Some(instrument(
                &market_id,
                raw,
                &market_id,
                &base,
                "AUSD",
                config
                    .get("price_decimals")
                    .and_then(Value::as_u64)
                    .and_then(decimal_tick),
                config
                    .get("size_decimals")
                    .and_then(Value::as_u64)
                    .and_then(decimal_tick),
            ))
        })
        .collect();

    Ok(venue(
        "perpl",
        "perpl",
        "wss://app.perpl.xyz/ws/v1/market-data",
        Some("order-book"),
        "AUSD",
        Some(PERPL_CONTEXT_URL),
        instruments,
    ))
}

async fn discover_ondo(client: &Client) -> anyhow::Result<VenueConfig> {
    let value = get_json(client, ONDO_MARKETS_URL).await?;
    let markets = array_at(
        &value,
        &["result", "perps", "tradingPairs"],
        "Ondo result.perps.tradingPairs",
    )?;
    let instruments = markets
        .iter()
        .filter(|market| market.get("disabled").and_then(Value::as_bool) != Some(true))
        .filter_map(|market| {
            let market_id = string_at(market, "market")?;
            let pair = market.get("pair")?;
            let base = normalize_base(string_at(pair, "base")?);
            let quote = string_at(pair, "quote")?;
            let raw = string_at(market, "displayName").unwrap_or(market_id);
            Some(instrument(
                market_id,
                raw,
                market_id,
                &base,
                quote,
                market
                    .get("quoteIncrement")
                    .and_then(Value::as_str)
                    .and_then(|value| value.parse().ok()),
                market
                    .get("baseIncrement")
                    .and_then(Value::as_str)
                    .and_then(|value| value.parse().ok()),
            ))
        })
        .collect();

    Ok(venue(
        "ondo",
        "ondo",
        "wss://api.ondoperps.xyz/ws",
        Some("topOfBooksPerps"),
        "USD",
        Some(ONDO_MARKETS_URL),
        instruments,
    ))
}

fn dedupe_instruments_by_base(instruments: Vec<InstrumentConfig>) -> Vec<InstrumentConfig> {
    let mut seen = HashSet::new();
    instruments
        .into_iter()
        .filter(|instrument| seen.insert(instrument.base_asset.clone()))
        .collect()
}

fn normalize_base(symbol: &str) -> String {
    let mut base = symbol.trim().to_ascii_uppercase();
    base = base
        .split_once('[')
        .map_or(base.clone(), |(value, _)| value.trim().to_string());
    if let Some((_, value)) = base.split_once(':') {
        base = value.to_string();
    }
    if let Some((value, _)) = base.split_once('/') {
        base = value.to_string();
    }
    if let Some((value, _)) = base.split_once('-') {
        base = value.to_string();
    }
    for suffix in ["USDC", "USD"] {
        if let Some(value) = base.strip_suffix(suffix)
            && !value.is_empty()
        {
            base = value.to_string();
            break;
        }
    }
    base
}

fn venue(
    venue_instance_id: &str,
    adapter: &str,
    url: &str,
    channel: Option<&str>,
    default_quote: &str,
    metadata_url: Option<&str>,
    instruments: Vec<InstrumentConfig>,
) -> VenueConfig {
    VenueConfig {
        venue_instance_id: venue_instance_id.to_string(),
        adapter: adapter.to_string(),
        enabled: true,
        url: Some(url.to_string()),
        channel: channel.map(str::to_string),
        catalog_source: CatalogSource::Config,
        metadata_url: metadata_url.map(str::to_string),
        default_quote_asset: default_quote.to_string(),
        default_settle_asset: default_quote.to_string(),
        default_margin_asset: default_quote.to_string(),
        instruments,
    }
}

#[allow(clippy::too_many_arguments)]
fn instrument(
    instrument_id: &str,
    raw_symbol: &str,
    feed_symbol: &str,
    base_asset: &str,
    quote_asset: &str,
    price_tick: Option<Fixed>,
    size_tick: Option<Fixed>,
) -> InstrumentConfig {
    InstrumentConfig {
        instrument_id: instrument_id.to_string(),
        raw_symbol: raw_symbol.to_string(),
        feed_symbol: Some(feed_symbol.to_string()),
        product_type: ProductType::Perp,
        base_asset: base_asset.to_ascii_uppercase(),
        quote_asset: Some(quote_asset.to_ascii_uppercase()),
        settle_asset: Some(quote_asset.to_ascii_uppercase()),
        margin_asset: Some(quote_asset.to_ascii_uppercase()),
        price_convention: PriceConvention::QuotePerBase,
        size_unit: SizeUnit::BaseAsset,
        price_tick,
        size_tick,
        min_size: None,
        status: "active".to_string(),
    }
}

fn decimal_tick(decimals: u64) -> Option<Fixed> {
    u32::try_from(decimals)
        .ok()
        .filter(|decimals| *decimals <= 18)
        .map(|decimals| Fixed::new(1, decimals))
}

async fn get_json(client: &Client, url: &str) -> anyhow::Result<Value> {
    retry_json(|| client.get(url).send(), url).await
}

async fn post_json(client: &Client, url: &str, body: Value) -> anyhow::Result<Value> {
    retry_json(|| client.post(url).json(&body).send(), url).await
}

async fn retry_json<F, Fut>(mut request: F, url: &str) -> anyhow::Result<Value>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<reqwest::Response, reqwest::Error>>,
{
    let mut last_error = None;
    for attempt in 0..3_u64 {
        let result = async {
            request()
                .await
                .with_context(|| format!("request {url}"))?
                .error_for_status()
                .with_context(|| format!("metadata returned an error status: {url}"))?
                .json()
                .await
                .with_context(|| format!("decode metadata JSON: {url}"))
        }
        .await;
        match result {
            Ok(value) => return Ok(value),
            Err(error) => last_error = Some(error),
        }
        if attempt < 2 {
            tokio::time::sleep(std::time::Duration::from_millis(250 * (attempt + 1))).await;
        }
    }
    Err(last_error.expect("at least one catalog request attempt"))
}

fn array_at<'a>(value: &'a Value, path: &[&str], label: &str) -> anyhow::Result<&'a Vec<Value>> {
    let mut current = value;
    for key in path {
        current = current
            .get(*key)
            .with_context(|| format!("{label} is missing `{key}`"))?;
    }
    current
        .as_array()
        .with_context(|| format!("{label} is not an array"))
}

fn string_at<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

fn scalar_string(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        dedupe_instruments_by_base, instrument, load_catalog_cache, normalize_base,
        parse_hyperliquid_instruments, save_catalog_cache, select_lighter_anchored_catalog, venue,
    };
    use serde_json::json;

    #[test]
    fn resolves_hyperliquid_collateral_and_excludes_unpriced_quotes() {
        let metas = json!([
            {
                "collateralToken": 0,
                "universe": [{"name": "BTC", "szDecimals": 5}]
            },
            {
                "collateralToken": 360,
                "universe": [{"name": "vntl:OPENAI", "szDecimals": 3}]
            }
        ]);
        let spot = json!({
            "tokens": [
                {"index": 0, "name": "USDC"},
                {"index": 360, "name": "USDH"}
            ]
        });

        let instruments = parse_hyperliquid_instruments(&metas, &spot).unwrap();

        assert_eq!(instruments.len(), 1);
        assert_eq!(instruments[0].instrument_id, "BTC");
        assert_eq!(instruments[0].quote_asset.as_deref(), Some("USDC"));
    }

    #[test]
    fn rejects_hyperliquid_metadata_without_collateral_identity() {
        let metas = json!([{"universe": [{"name": "BTC"}]}]);
        let spot = json!({"tokens": [{"index": 0, "name": "USDC"}]});

        assert!(parse_hyperliquid_instruments(&metas, &spot).is_err());
    }

    #[test]
    fn dedupes_one_instrument_per_base_for_a_venue() {
        let values = vec![
            instrument("1", "BTC", "1", "BTC", "USDC", None, None),
            instrument("2", "BTC-PERP", "2", "BTC", "USDC", None, None),
            instrument("3", "BTC-USD", "3", "BTC", "USD", None, None),
        ];
        assert_eq!(dedupe_instruments_by_base(values).len(), 1);
    }

    #[test]
    fn normalizes_exchange_specific_symbols_to_the_anchor_base() {
        assert_eq!(normalize_base("xyz:SPCX"), "SPCX");
        assert_eq!(normalize_base("BTC/USDC"), "BTC");
        assert_eq!(normalize_base("ETH-USD"), "ETH");
        assert_eq!(normalize_base("SOLUSD"), "SOL");
    }

    #[test]
    fn selects_cross_quote_markets_only_when_they_exist_on_lighter() {
        let mut venues = vec![
            venue(
                "lighter",
                "lighter",
                "wss://example.test",
                None,
                "USDC",
                None,
                vec![
                    instrument("1", "BTC", "1", "BTC", "USDC", None, None),
                    instrument("2", "ETH", "2", "ETH", "USDC", None, None),
                ],
            ),
            venue(
                "usd",
                "test",
                "wss://example.test",
                None,
                "USD",
                None,
                vec![
                    instrument("BTC", "BTCUSD", "BTC", "BTC", "USD", None, None),
                    instrument("SOL", "SOLUSD", "SOL", "SOL", "USD", None, None),
                ],
            ),
        ];

        assert_eq!(select_lighter_anchored_catalog(&mut venues).unwrap(), 1);
        assert_eq!(venues.len(), 2);
        assert_eq!(venues[0].instruments[0].base_asset, "BTC");
        assert_eq!(venues[1].instruments[0].quote_asset.as_deref(), Some("USD"));
    }

    #[tokio::test]
    async fn catalog_cache_round_trips_the_last_known_good_plan() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("nested/catalog.json");
        let venues = vec![venue(
            "lighter",
            "lighter",
            "wss://example.test",
            None,
            "USDC",
            None,
            vec![instrument("1", "BTC", "1", "BTC", "USDC", None, None)],
        )];

        save_catalog_cache(&path, &venues).await.unwrap();
        assert_eq!(load_catalog_cache(&path).await.unwrap(), venues);
    }
}
