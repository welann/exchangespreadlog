use std::{fmt, fs, path::Path, str::FromStr};

use anyhow::Result;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub mode: String,
    pub pipeline: PipelineConfig,
    pub storage: StorageConfig,
    pub tui: TuiConfig,
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
    pub name: String,
    pub enabled: bool,
    pub url: Option<String>,
    pub markets: Vec<String>,
    pub channel: Option<String>,
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
}

impl Default for Config {
    fn default() -> Self {
        Self {
            mode: "bbo".to_string(),
            pipeline: PipelineConfig::default(),
            storage: StorageConfig::default(),
            tui: TuiConfig::default(),
            venues: vec![
                VenueConfig {
                    name: "hyperliquid".to_string(),
                    enabled: true,
                    url: Some("wss://api.hyperliquid.xyz/ws".to_string()),
                    markets: vec!["BTC".to_string(), "ETH".to_string(), "SOL".to_string()],
                    channel: Some("bbo".to_string()),
                },
                VenueConfig {
                    name: "lighter".to_string(),
                    enabled: true,
                    url: Some("wss://mainnet.zklighter.elliot.ai/stream".to_string()),
                    markets: vec!["0".to_string(), "1".to_string(), "2".to_string()],
                    channel: Some("ticker".to_string()),
                },
                VenueConfig {
                    name: "risex".to_string(),
                    enabled: true,
                    url: Some("wss://ws.rise.trade/ws".to_string()),
                    markets: vec![
                        "1:BTC".to_string(),
                        "2:ETH".to_string(),
                        "4:SOL".to_string(),
                    ],
                    channel: Some("orderbook".to_string()),
                },
                VenueConfig {
                    name: "01".to_string(),
                    enabled: true,
                    url: Some("wss://zo-mainnet.n1.xyz".to_string()),
                    markets: vec![
                        "0:BTC:BTCUSD".to_string(),
                        "1:ETH:ETHUSD".to_string(),
                        "2:SOL:SOLUSD".to_string(),
                    ],
                    channel: Some("deltas".to_string()),
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
            name: String::new(),
            enabled: true,
            url: None,
            markets: Vec::new(),
            channel: None,
        }
    }
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
        assert_eq!(config.venues.len(), 4);
        assert_eq!(config.venues[0].name, "hyperliquid");
        assert_eq!(config.venues[0].channel.as_deref(), Some("bbo"));
        assert_eq!(config.venues[1].name, "lighter");
        assert_eq!(config.venues[1].channel.as_deref(), Some("ticker"));
        assert!(config.venues[1].markets.contains(&"2".to_string()));
        assert_eq!(config.venues[2].name, "risex");
        assert_eq!(
            config.venues[2].url.as_deref(),
            Some("wss://ws.rise.trade/ws")
        );
        assert_eq!(config.venues[2].channel.as_deref(), Some("orderbook"));
        assert!(config.venues[2].markets.contains(&"4:SOL".to_string()));
        assert_eq!(config.venues[3].name, "01");
        assert_eq!(
            config.venues[3].url.as_deref(),
            Some("wss://zo-mainnet.n1.xyz")
        );
        assert_eq!(config.venues[3].channel.as_deref(), Some("deltas"));
        assert!(
            config.venues[3]
                .markets
                .contains(&"2:SOL:SOLUSD".to_string())
        );
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
