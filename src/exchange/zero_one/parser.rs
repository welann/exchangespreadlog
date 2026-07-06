use std::str::FromStr;

use anyhow::{Result, anyhow};
use serde::Deserialize;
use serde_json::Value;

use crate::domain::{BestLevel, Fixed};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrderbookSnapshot {
    pub update_id: i128,
    pub bids: Vec<BestLevel>,
    pub asks: Vec<BestLevel>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrderbookDelta {
    pub market_symbol: String,
    pub last_update_id: i128,
    pub update_id: i128,
    pub bids: Vec<BestLevel>,
    pub asks: Vec<BestLevel>,
}

#[derive(Debug, Deserialize)]
struct SnapshotEnvelope {
    #[serde(rename = "updateId")]
    update_id: i128,
    #[serde(default)]
    bids: Vec<[Value; 2]>,
    #[serde(default)]
    asks: Vec<[Value; 2]>,
}

#[derive(Debug, Deserialize)]
struct DeltaEnvelope {
    delta: Option<DeltaPayload>,
}

#[derive(Debug, Deserialize)]
struct DeltaPayload {
    market_symbol: String,
    #[serde(default)]
    last_update_id: Option<i128>,
    #[serde(default)]
    update_id: Option<i128>,
    #[serde(default)]
    bids: Vec<[Value; 2]>,
    #[serde(default)]
    asks: Vec<[Value; 2]>,
}

pub fn parse_snapshot(text: &str) -> Result<OrderbookSnapshot> {
    let snapshot: SnapshotEnvelope = serde_json::from_str(text)?;
    Ok(OrderbookSnapshot {
        update_id: snapshot.update_id,
        bids: parse_levels(snapshot.bids)?,
        asks: parse_levels(snapshot.asks)?,
    })
}

pub fn parse_delta(text: &str) -> Result<Option<OrderbookDelta>> {
    let envelope: DeltaEnvelope = serde_json::from_str(text)?;
    let Some(delta) = envelope.delta else {
        return Ok(None);
    };

    Ok(Some(OrderbookDelta {
        market_symbol: delta.market_symbol,
        last_update_id: delta
            .last_update_id
            .ok_or_else(|| anyhow!("01 delta missing last_update_id"))?,
        update_id: delta
            .update_id
            .ok_or_else(|| anyhow!("01 delta missing update_id"))?,
        bids: parse_levels(delta.bids)?,
        asks: parse_levels(delta.asks)?,
    }))
}

fn parse_levels(levels: Vec<[Value; 2]>) -> Result<Vec<BestLevel>> {
    levels
        .into_iter()
        .map(|[price, size]| {
            Ok(BestLevel::new(
                value_to_fixed(&price).map_err(|err| anyhow!("invalid 01 price: {err}"))?,
                value_to_fixed(&size).map_err(|err| anyhow!("invalid 01 size: {err}"))?,
                None,
            ))
        })
        .collect()
}

fn value_to_fixed(value: &Value) -> Result<Fixed> {
    match value {
        Value::String(value) => Fixed::from_str(value),
        Value::Number(value) => Fixed::from_str(&value.to_string()),
        other => anyhow::bail!("expected string or number, got {other}"),
    }
}

#[cfg(test)]
mod tests {
    use super::{parse_delta, parse_snapshot};

    #[test]
    fn parses_rest_orderbook_snapshot() {
        let raw = r#"{
            "updateId": 5568721804,
            "asks": [[61856.3, 0.00808], [61860.1, 0.03251]],
            "bids": [[61849.9, 0.05004], [61848.4, 0.00014]],
            "asksSummary": {"sum": 5.70116, "count": 115},
            "bidsSummary": {"sum": 6.33777, "count": 104}
        }"#;

        let snapshot = parse_snapshot(raw).unwrap();

        assert_eq!(snapshot.update_id, 5_568_721_804);
        assert_eq!(snapshot.bids[0].price.to_string(), "61849.9");
        assert_eq!(snapshot.bids[0].size.to_string(), "0.05004");
        assert_eq!(snapshot.asks[0].price.to_string(), "61856.3");
    }

    #[test]
    fn parses_websocket_delta() {
        let raw = r#"{
            "delta": {
                "last_update_id": 5568740203,
                "update_id": 5568740302,
                "market_symbol": "BTCUSD",
                "asks": [[62002.9, 0.015], [61987.8, 0.0]],
                "bids": [[61923.5, 0.0], [61866.9, 0.00064]]
            }
        }"#;

        let delta = parse_delta(raw).unwrap().unwrap();

        assert_eq!(delta.market_symbol, "BTCUSD");
        assert_eq!(delta.last_update_id, 5_568_740_203);
        assert_eq!(delta.update_id, 5_568_740_302);
        assert_eq!(delta.asks[0].price.to_string(), "62002.9");
        assert_eq!(delta.asks[1].size.to_string(), "0");
        assert_eq!(delta.bids[1].size.to_string(), "0.00064");
    }

    #[test]
    fn ignores_non_delta_messages() {
        assert!(parse_delta(r#"{"type":"pong"}"#).unwrap().is_none());
    }
}
