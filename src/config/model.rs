use std::{fs, path::Path};

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
#[serde(default)]
pub struct StorageConfig {
    pub jsonl_enabled: bool,
    pub jsonl_dir: String,
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
            jsonl_enabled: true,
            jsonl_dir: "data/bbo".to_string(),
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
    use super::Config;

    #[test]
    fn default_config_round_trips_through_toml() {
        let toml = Config::default_toml().unwrap();
        let config: Config = toml::from_str(&toml).unwrap();

        assert_eq!(config.mode, "bbo");
        assert!(config.tui.enabled);
        assert_eq!(config.tui.refresh_ms, 250);
        assert_eq!(config.venues.len(), 2);
        assert_eq!(config.venues[0].name, "hyperliquid");
        assert_eq!(config.venues[0].channel.as_deref(), Some("bbo"));
        assert_eq!(config.venues[1].name, "lighter");
        assert_eq!(config.venues[1].channel.as_deref(), Some("ticker"));
        assert!(config.venues[1].markets.contains(&"2".to_string()));
    }
}
