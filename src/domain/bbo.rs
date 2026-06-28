use serde::{Deserialize, Serialize};

use super::{DataQuality, Fixed, MarketRef, SourceKind, Venue};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BestLevel {
    pub price: Fixed,
    pub size: Fixed,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order_count: Option<u32>,
}

impl BestLevel {
    pub fn new(price: Fixed, size: Fixed, order_count: Option<u32>) -> Self {
        Self {
            price,
            size,
            order_count,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BboTick {
    pub venue: Venue,
    pub market: MarketRef,
    pub recv_ts_ns: i128,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exchange_ts_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sequence: Option<i128>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bid: Option<BestLevel>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ask: Option<BestLevel>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spread: Option<Fixed>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mid: Option<Fixed>,
    pub source: SourceKind,
    #[serde(default)]
    pub quality: DataQuality,
}

impl BboTick {
    pub fn new(
        venue: Venue,
        market: MarketRef,
        recv_ts_ns: i128,
        exchange_ts_ms: Option<i64>,
        sequence: Option<i128>,
        bid: Option<BestLevel>,
        ask: Option<BestLevel>,
        source: SourceKind,
    ) -> Self {
        Self {
            venue,
            market,
            recv_ts_ns,
            exchange_ts_ms,
            sequence,
            bid,
            ask,
            spread: None,
            mid: None,
            source,
            quality: DataQuality::default(),
        }
    }
}
