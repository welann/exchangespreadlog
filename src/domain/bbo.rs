use serde::{Deserialize, Serialize};

use super::{DataQuality, Fixed, InstrumentCatalog, InstrumentRef, SourceKind};

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
    pub instrument: InstrumentRef,
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
        instrument: InstrumentRef,
        recv_ts_ns: i128,
        exchange_ts_ms: Option<i64>,
        sequence: Option<i128>,
        bid: Option<BestLevel>,
        ask: Option<BestLevel>,
        source: SourceKind,
    ) -> Self {
        Self {
            instrument,
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MarketEvent {
    Catalog { instrument: InstrumentCatalog },
    Tick { tick: BboTick },
}
