use std::{fmt, fs, path::Path, str::FromStr};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::domain::{Fixed, InstrumentCatalog, ProductType, QuoteRate, QuoteRateBook};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub mode: String,
    pub pipeline: PipelineConfig,
    pub storage: StorageConfig,
    pub tui: TuiConfig,
    pub quote_rates: Vec<QuoteRateConfig>,
    pub venues: Vec<VenueConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PipelineConfig {
    pub channel_capacity: usize,
    pub stale_after_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<StorageMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jsonl_enabled: Option<bool>,
    #[serde(default = "default_jsonl_dir")]
    pub jsonl_dir: String,
    #[serde(default)]
    pub clickhouse: ClickHouseConfig,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StorageMode {
    None,
    Jsonl,
    Clickhouse,
    Both,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ClickHouseConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    pub url: String,
    pub database: String,
    pub table: String,
    pub catalog_table: String,
    pub username: String,
    pub password: Option<String>,
    pub password_env: Option<String>,
    pub create_table: bool,
    pub batch_size: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TuiConfig {
    pub enabled: bool,
    pub refresh_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct VenueConfig {
    pub venue_instance_id: String,
    pub adapter: String,
    pub enabled: bool,
    pub url: Option<String>,
    pub channel: Option<String>,
    pub default_quote_asset: String,
    pub default_settle_asset: String,
    pub default_margin_asset: String,
    pub instruments: Vec<InstrumentConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstrumentConfig {
    pub instrument_id: String,
    pub raw_symbol: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub feed_symbol: Option<String>,
    pub product_type: ProductType,
    pub base_asset: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quote_asset: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settle_asset: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub margin_asset: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub price_tick: Option<Fixed>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_tick: Option<Fixed>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_size: Option<Fixed>,
    #[serde(default = "default_instrument_status")]
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuoteRateConfig {
    pub from: String,
    pub to: String,
    pub rate: Fixed,
}

impl Config {
    pub fn load_or_default(path: &Path) -> Result<Self> {
        if path.exists() {
            let raw = fs::read_to_string(path)?;
            Ok(toml::from_str(&raw)?)
        } else {
            Ok(Self::default())
        }
    }

    pub fn default_toml() -> Result<String> {
        Ok(toml::to_string_pretty(&Self::default())?)
    }

    pub fn quote_rate_book(&self) -> QuoteRateBook {
        QuoteRateBook::new(self.quote_rates.iter().map(|rate| QuoteRate {
            from: rate.from.clone(),
            to: rate.to.clone(),
            rate: rate.rate,
        }))
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            mode: "bbo".to_string(),
            pipeline: PipelineConfig::default(),
            storage: StorageConfig::default(),
            tui: TuiConfig::default(),
            quote_rates: vec![
                QuoteRateConfig {
                    from: "USDC".to_string(),
                    to: "USD".to_string(),
                    rate: "1".parse().expect("valid default USDC rate"),
                },
                QuoteRateConfig {
                    from: "USDT".to_string(),
                    to: "USD".to_string(),
                    rate: "1".parse().expect("valid default USDT rate"),
                },
            ],
            venues: vec![
                VenueConfig {
                    venue_instance_id: "hyperliquid".to_string(),
                    adapter: "hyperliquid".to_string(),
                    enabled: true,
                    url: Some("wss://api.hyperliquid.xyz/ws".to_string()),
                    channel: Some("bbo".to_string()),
                    default_quote_asset: "USDC".to_string(),
                    default_settle_asset: "USDC".to_string(),
                    default_margin_asset: "USDC".to_string(),
                    instruments: default_hyperliquid_instruments(),
                },
                VenueConfig {
                    venue_instance_id: "lighter".to_string(),
                    adapter: "lighter".to_string(),
                    enabled: true,
                    url: Some("wss://mainnet.zklighter.elliot.ai/stream".to_string()),
                    channel: Some("ticker".to_string()),
                    default_quote_asset: "USDC".to_string(),
                    default_settle_asset: "USDC".to_string(),
                    default_margin_asset: "USDC".to_string(),
                    instruments: default_lighter_instruments(),
                },
                VenueConfig {
                    venue_instance_id: "risex".to_string(),
                    adapter: "risex".to_string(),
                    enabled: true,
                    url: Some("wss://ws.rise.trade/ws".to_string()),
                    channel: Some("orderbook".to_string()),
                    default_quote_asset: "USDC".to_string(),
                    default_settle_asset: "USDC".to_string(),
                    default_margin_asset: "USDC".to_string(),
                    instruments: default_risex_instruments(),
                },
                VenueConfig {
                    venue_instance_id: "01".to_string(),
                    adapter: "01".to_string(),
                    enabled: true,
                    url: Some("wss://zo-mainnet.n1.xyz".to_string()),
                    channel: Some("deltas".to_string()),
                    default_quote_asset: "USD".to_string(),
                    default_settle_asset: "USD".to_string(),
                    default_margin_asset: "USD".to_string(),
                    instruments: default_zero_one_instruments(),
                },
            ],
        }
    }
}

impl Default for PipelineConfig {
    fn default() -> Self {
        Self {
            channel_capacity: 4096,
            stale_after_ms: 5_000,
        }
    }
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            mode: Some(StorageMode::Jsonl),
            jsonl_enabled: None,
            jsonl_dir: "data/bbo".to_string(),
            clickhouse: ClickHouseConfig::default(),
        }
    }
}

fn default_jsonl_dir() -> String {
    "data/bbo".to_string()
}

impl StorageConfig {
    pub fn effective_mode(&self) -> StorageMode {
        if let Some(mode) = self.mode {
            return mode;
        }

        match (
            self.jsonl_enabled.unwrap_or(true),
            self.clickhouse.enabled.unwrap_or(false),
        ) {
            (false, false) => StorageMode::None,
            (true, false) => StorageMode::Jsonl,
            (false, true) => StorageMode::Clickhouse,
            (true, true) => StorageMode::Both,
        }
    }
}

impl StorageMode {
    pub const VALUES: [&'static str; 4] = ["none", "jsonl", "clickhouse", "both"];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Jsonl => "jsonl",
            Self::Clickhouse => "clickhouse",
            Self::Both => "both",
        }
    }
}

impl FromStr for StorageMode {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "none" | "noop" | "off" => Ok(Self::None),
            "jsonl" | "local" => Ok(Self::Jsonl),
            "clickhouse" | "ch" => Ok(Self::Clickhouse),
            "both" | "all" => Ok(Self::Both),
            other => Err(format!(
                "unsupported storage mode `{other}`; expected one of: {}",
                Self::VALUES.join(", ")
            )),
        }
    }
}

impl fmt::Display for StorageMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Default for ClickHouseConfig {
    fn default() -> Self {
        Self {
            enabled: None,
            url: "https://obdata.zeabur.app/".to_string(),
            database: "zeabur".to_string(),
            table: "bbo_ticks".to_string(),
            catalog_table: "instrument_catalog".to_string(),
            username: "zeabur".to_string(),
            password: None,
            password_env: Some("CLICKHOUSE_PASSWORD".to_string()),
            create_table: true,
            batch_size: 100,
        }
    }
}

impl Default for VenueConfig {
    fn default() -> Self {
        Self {
            venue_instance_id: String::new(),
            adapter: String::new(),
            enabled: true,
            url: None,
            channel: None,
            default_quote_asset: "USD".to_string(),
            default_settle_asset: "USD".to_string(),
            default_margin_asset: "USD".to_string(),
            instruments: Vec::new(),
        }
    }
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
        InstrumentCatalog::new(
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
            self.price_tick,
            self.size_tick,
            self.min_size,
            self.status.clone(),
            None,
        )
    }
}

fn default_instrument_status() -> String {
    "active".to_string()
}

fn instrument(
    instrument_id: &str,
    raw_symbol: &str,
    feed_symbol: Option<&str>,
    base_asset: &str,
) -> InstrumentConfig {
    InstrumentConfig {
        instrument_id: instrument_id.to_string(),
        raw_symbol: raw_symbol.to_string(),
        feed_symbol: feed_symbol.map(str::to_string),
        product_type: ProductType::Perp,
        base_asset: base_asset.to_string(),
        quote_asset: None,
        settle_asset: None,
        margin_asset: None,
        price_tick: None,
        size_tick: None,
        min_size: None,
        status: default_instrument_status(),
    }
}

fn default_hyperliquid_instruments() -> Vec<InstrumentConfig> {
    vec![
        instrument("BTC", "BTC", Some("BTC"), "BTC"),
        instrument("ETH", "ETH", Some("ETH"), "ETH"),
        instrument("SOL", "SOL", Some("SOL"), "SOL"),
    ]
}

fn default_lighter_instruments() -> Vec<InstrumentConfig> {
    vec![
        instrument("0", "BTC", Some("0"), "BTC"),
        instrument("1", "ETH", Some("1"), "ETH"),
        instrument("2", "SOL", Some("2"), "SOL"),
    ]
}

fn default_risex_instruments() -> Vec<InstrumentConfig> {
    vec![
        instrument("1", "BTC", Some("1"), "BTC"),
        instrument("2", "ETH", Some("2"), "ETH"),
        instrument("4", "SOL", Some("4"), "SOL"),
    ]
}

fn default_zero_one_instruments() -> Vec<InstrumentConfig> {
    vec![
        instrument("0", "BTCUSD", Some("BTCUSD"), "BTC"),
        instrument("1", "ETHUSD", Some("ETHUSD"), "ETH"),
        instrument("2", "SOLUSD", Some("SOLUSD"), "SOL"),
    ]
}

impl Default for TuiConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            refresh_ms: 250,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Config, StorageConfig, StorageMode};

    #[test]
    fn default_config_round_trips_through_toml() {
        let toml = Config::default_toml().unwrap();
        let config: Config = toml::from_str(&toml).unwrap();

        assert_eq!(config.mode, "bbo");
        assert!(config.tui.enabled);
        assert_eq!(config.tui.refresh_ms, 250);
        assert_eq!(config.storage.mode, Some(StorageMode::Jsonl));
        assert_eq!(config.storage.effective_mode(), StorageMode::Jsonl);
        assert_eq!(config.storage.clickhouse.enabled, None);
        assert_eq!(config.storage.clickhouse.url, "https://obdata.zeabur.app/");
        assert_eq!(config.storage.clickhouse.table, "bbo_ticks");
        assert_eq!(
            config.storage.clickhouse.catalog_table,
            "instrument_catalog"
        );
        assert_eq!(config.quote_rates.len(), 2);
        assert_eq!(config.venues.len(), 4);
        assert_eq!(config.venues[0].venue_instance_id, "hyperliquid");
        assert_eq!(config.venues[0].adapter, "hyperliquid");
        assert_eq!(config.venues[0].channel.as_deref(), Some("bbo"));
        assert_eq!(config.venues[0].instruments[0].base_asset, "BTC");
        assert_eq!(config.venues[0].catalog()[0].quote_asset, "USDC");
        assert_eq!(config.venues[1].venue_instance_id, "lighter");
        assert_eq!(config.venues[1].channel.as_deref(), Some("ticker"));
        assert_eq!(config.venues[1].instruments[2].instrument_id, "2");
        assert_eq!(config.venues[2].venue_instance_id, "risex");
        assert_eq!(
            config.venues[2].url.as_deref(),
            Some("wss://ws.rise.trade/ws")
        );
        assert_eq!(config.venues[2].channel.as_deref(), Some("orderbook"));
        assert_eq!(config.venues[2].instruments[2].base_asset, "SOL");
        assert_eq!(config.venues[3].venue_instance_id, "01");
        assert_eq!(
            config.venues[3].url.as_deref(),
            Some("wss://zo-mainnet.n1.xyz")
        );
        assert_eq!(config.venues[3].channel.as_deref(), Some("deltas"));
        assert_eq!(config.venues[3].catalog()[2].feed_key(), "SOLUSD");
    }

    #[test]
    fn legacy_storage_flags_still_select_effective_mode() {
        let config: StorageConfig = toml::from_str(
            r#"
jsonl_enabled = false

[clickhouse]
enabled = true
"#,
        )
        .unwrap();

        assert_eq!(config.mode, None);
        assert_eq!(config.effective_mode(), StorageMode::Clickhouse);
    }

    #[test]
    fn parses_storage_mode_aliases() {
        assert_eq!("none".parse::<StorageMode>().unwrap(), StorageMode::None);
        assert_eq!("local".parse::<StorageMode>().unwrap(), StorageMode::Jsonl);
        assert_eq!(
            "clickhouse".parse::<StorageMode>().unwrap(),
            StorageMode::Clickhouse
        );
        assert!("sqlite".parse::<StorageMode>().is_err());
    }
}
