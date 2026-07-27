use std::{
    collections::HashSet,
    env,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::PathBuf,
    time::Duration,
};

use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};

use crate::domain::{
    Fixed, InstrumentCatalog, PriceConvention, ProductType, QuoteRate, QuoteRateBook, SizeUnit,
};

#[derive(Debug, Clone)]
pub struct RuntimeConfig {
    pub http_addr: SocketAddr,
    pub clickhouse: ClickHouseConfig,
    pub wal_path: PathBuf,
    pub web_dir: PathBuf,
    pub stale_after_ms: i64,
    pub query_max_book_age_ms: i64,
    pub quote_rates: QuoteRateBook,
    pub catalog_cache_path: PathBuf,
    pub catalog_refresh_interval: Duration,
    pub disabled_venues: HashSet<String>,
}

#[derive(Debug, Clone)]
pub struct ClickHouseConfig {
    pub url: String,
    pub database: String,
    pub username: String,
    pub password: String,
    pub projector_batch_size: usize,
    pub projector_linger: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VenueConfig {
    pub venue_instance_id: String,
    pub adapter: String,
    pub enabled: bool,
    pub url: Option<String>,
    pub channel: Option<String>,
    pub catalog_source: CatalogSource,
    pub metadata_url: Option<String>,
    pub default_quote_asset: String,
    pub default_settle_asset: String,
    pub default_margin_asset: String,
    pub instruments: Vec<InstrumentConfig>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CatalogSource {
    Config,
    Exchange,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstrumentConfig {
    pub instrument_id: String,
    pub raw_symbol: String,
    pub feed_symbol: Option<String>,
    pub product_type: ProductType,
    pub base_asset: String,
    pub quote_asset: Option<String>,
    pub settle_asset: Option<String>,
    pub margin_asset: Option<String>,
    pub price_convention: PriceConvention,
    pub size_unit: SizeUnit,
    pub price_tick: Option<Fixed>,
    pub size_tick: Option<Fixed>,
    pub min_size: Option<Fixed>,
    pub status: String,
}

impl RuntimeConfig {
    pub fn from_env() -> anyhow::Result<Self> {
        let _ = dotenvy::dotenv();

        let clickhouse_url = required("CLICKHOUSE_URL")?
            .trim_end_matches('/')
            .to_string();
        if !(clickhouse_url.starts_with("https://") || clickhouse_url.starts_with("http://")) {
            bail!("CLICKHOUSE_URL must start with http:// or https://");
        }

        let host = env::var("HTTP_HOST")
            .unwrap_or_else(|_| "0.0.0.0".to_string())
            .parse::<IpAddr>()
            .context("parse HTTP_HOST")?;
        let port = env::var("HTTP_PORT")
            .unwrap_or_else(|_| "8080".to_string())
            .parse::<u16>()
            .context("parse HTTP_PORT")?;
        let catalog_refresh_seconds = parse_env("CATALOG_REFRESH_SECONDS", 3_600_u64)?;
        if catalog_refresh_seconds == 0 {
            bail!("CATALOG_REFRESH_SECONDS must be greater than zero");
        }
        let clickhouse_projector_batch_size =
            parse_env("CLICKHOUSE_PROJECTOR_BATCH_SIZE", 5_000_usize)?;
        if clickhouse_projector_batch_size == 0 {
            bail!("CLICKHOUSE_PROJECTOR_BATCH_SIZE must be greater than zero");
        }
        let clickhouse_projector_linger_ms =
            parse_env("CLICKHOUSE_PROJECTOR_LINGER_MS", 5_000_u64)?;
        if clickhouse_projector_linger_ms == 0 {
            bail!("CLICKHOUSE_PROJECTOR_LINGER_MS must be greater than zero");
        }

        Ok(Self {
            http_addr: SocketAddr::new(host, port),
            clickhouse: ClickHouseConfig {
                url: clickhouse_url,
                database: env::var("CLICKHOUSE_DB").unwrap_or_else(|_| "zeabur".to_string()),
                username: required("CLICKHOUSE_USER")?,
                password: required("CLICKHOUSE_PASSWORD")?,
                projector_batch_size: clickhouse_projector_batch_size,
                projector_linger: Duration::from_millis(clickhouse_projector_linger_ms),
            },
            wal_path: PathBuf::from(
                env::var("WAL_PATH").unwrap_or_else(|_| "data/wal.sqlite3".to_string()),
            ),
            web_dir: PathBuf::from(env::var("WEB_DIR").unwrap_or_else(|_| "web/build".to_string())),
            stale_after_ms: parse_env("STALE_AFTER_MS", 5_000)?,
            query_max_book_age_ms: parse_env("QUERY_MAX_BOOK_AGE_MS", 30_000)?,
            quote_rates: default_quote_rates(),
            catalog_cache_path: PathBuf::from(
                env::var("CATALOG_CACHE_PATH").unwrap_or_else(|_| "data/catalog.json".to_string()),
            ),
            catalog_refresh_interval: Duration::from_secs(catalog_refresh_seconds),
            disabled_venues: parse_csv_env("DISABLED_VENUES"),
        })
    }

    pub fn development_without_secrets() -> Self {
        Self {
            http_addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 8080),
            clickhouse: ClickHouseConfig {
                url: "http://127.0.0.1:8123".to_string(),
                database: "zeabur".to_string(),
                username: "default".to_string(),
                password: String::new(),
                projector_batch_size: 5_000,
                projector_linger: Duration::from_secs(5),
            },
            wal_path: PathBuf::from("data/wal.sqlite3"),
            web_dir: PathBuf::from("web/build"),
            stale_after_ms: 5_000,
            query_max_book_age_ms: 30_000,
            quote_rates: default_quote_rates(),
            catalog_cache_path: PathBuf::from("data/catalog.json"),
            catalog_refresh_interval: Duration::from_secs(3_600),
            disabled_venues: HashSet::new(),
        }
    }
}

fn parse_csv_env(name: &str) -> HashSet<String> {
    env::var(name)
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_ascii_lowercase())
        .collect()
}

fn default_quote_rates() -> QuoteRateBook {
    QuoteRateBook::new(["USDC", "USDT", "AUSD"].into_iter().map(|from| QuoteRate {
        from: from.to_string(),
        to: "USD".to_string(),
        rate: Fixed::new(1, 0),
    }))
}

impl VenueConfig {
    pub fn catalog(&self) -> Vec<InstrumentCatalog> {
        self.instruments
            .iter()
            .map(|instrument| instrument.to_catalog(self))
            .collect()
    }
}

impl InstrumentConfig {
    pub fn to_catalog(&self, venue: &VenueConfig) -> InstrumentCatalog {
        InstrumentCatalog::new_with_units(
            venue.venue_instance_id.clone(),
            self.instrument_id.clone(),
            self.raw_symbol.clone(),
            self.feed_symbol.clone(),
            self.product_type,
            self.base_asset.clone(),
            self.quote_asset
                .clone()
                .unwrap_or_else(|| venue.default_quote_asset.clone()),
            self.settle_asset
                .clone()
                .unwrap_or_else(|| venue.default_settle_asset.clone()),
            self.margin_asset
                .clone()
                .unwrap_or_else(|| venue.default_margin_asset.clone()),
            self.price_convention,
            self.size_unit,
            self.price_tick,
            self.size_tick,
            self.min_size,
            self.status.clone(),
            None,
        )
    }
}

fn required(name: &str) -> anyhow::Result<String> {
    let value = env::var(name).with_context(|| format!("{name} is required"))?;
    if value.trim().is_empty() {
        bail!("{name} cannot be empty");
    }
    Ok(value)
}

fn parse_env<T>(name: &str, default: T) -> anyhow::Result<T>
where
    T: std::str::FromStr,
    T::Err: std::error::Error + Send + Sync + 'static,
{
    match env::var(name) {
        Ok(value) => value.parse().with_context(|| format!("parse {name}")),
        Err(_) => Ok(default),
    }
}
