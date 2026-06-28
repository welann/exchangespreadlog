use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Venue {
    Hyperliquid,
    Lighter,
    Risex,
}

impl Venue {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Hyperliquid => "hyperliquid",
            Self::Lighter => "lighter",
            Self::Risex => "risex",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MarketRef {
    pub id: String,
    pub symbol: Option<String>,
}

impl MarketRef {
    pub fn new(id: impl Into<String>, symbol: Option<String>) -> Self {
        Self {
            id: id.into(),
            symbol,
        }
    }

    pub fn label(&self) -> &str {
        self.symbol.as_deref().unwrap_or(&self.id)
    }
}
