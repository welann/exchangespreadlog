use std::str::FromStr;

use anyhow::{Result, anyhow, bail};
use serde::Deserialize;
use serde_json::Value;

use crate::domain::{BestLevel, Fixed};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParsedMessage {
    L2Book(L2BookUpdate),
    Ignore,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct L2BookUpdate {
    pub symbol: String,
    pub exchange_ts_ms: i64,
    pub previous_ts_ms: Option<i64>,
    pub is_snapshot: bool,
    pub bids: Vec<EtherealLevelDelta>,
    pub asks: Vec<EtherealLevelDelta>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EtherealLevelDelta {
    pub level: BestLevel,
    pub raw_price: String,
    pub raw_size: String,
}

#[derive(Debug, Deserialize)]
struct Envelope {
    #[serde(default)]
    e: Option<String>,
    #[serde(default)]
    event: Option<String>,
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    data: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct L2BookData {
    #[serde(rename = "s")]
    symbol: String,
    #[serde(rename = "t")]
    timestamp_ms: i64,
    #[serde(default, rename = "pt")]
    previous_timestamp_ms: Option<i64>,
    #[serde(default, rename = "b")]
    bids: Vec<[Value; 2]>,
    #[serde(default, rename = "a")]
    asks: Vec<[Value; 2]>,
}

pub fn parse_message(text: &str) -> Result<ParsedMessage> {
    let envelope: Envelope = serde_json::from_str(text)?;

    if envelope.event.as_deref() == Some("error") {
        bail!(
            "Ethereal websocket error: {}",
            envelope.message.unwrap_or_else(|| text.to_string())
        );
    }

    match envelope.e.as_deref() {
        Some("L2Book") => {
            let data = envelope
                .data
                .ok_or_else(|| anyhow!("Ethereal L2Book message missing data"))?;
            parse_l2_book(data).map(ParsedMessage::L2Book)
        }
        _ => Ok(ParsedMessage::Ignore),
    }
}

fn parse_l2_book(data: Value) -> Result<L2BookUpdate> {
    let payload: L2BookData = serde_json::from_value(data)?;
    let previous_ts_ms = payload.previous_timestamp_ms;

    Ok(L2BookUpdate {
        symbol: payload.symbol,
        exchange_ts_ms: payload.timestamp_ms,
        previous_ts_ms,
        is_snapshot: previous_ts_ms.is_none(),
        bids: parse_levels(payload.bids)?,
        asks: parse_levels(payload.asks)?,
    })
}

fn parse_levels(levels: Vec<[Value; 2]>) -> Result<Vec<EtherealLevelDelta>> {
    levels
        .into_iter()
        .map(|[price, size]| {
            let raw_price = decimal_value_to_string(price)?;
            let raw_size = decimal_value_to_string(size)?;
            Ok(EtherealLevelDelta {
                level: BestLevel::new(
                    Fixed::from_str(&raw_price)
                        .map_err(|err| anyhow!("invalid Ethereal price: {err}"))?,
                    Fixed::from_str(&raw_size)
                        .map_err(|err| anyhow!("invalid Ethereal quantity: {err}"))?,
                    None,
                ),
                raw_price,
                raw_size,
            })
        })
        .collect()
}

fn decimal_value_to_string(value: Value) -> Result<String> {
    match value {
        Value::String(value) if !value.trim().is_empty() => Ok(value),
        Value::Number(value) => Ok(value.to_string()),
        other => Err(anyhow!("expected decimal string or number, got {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::{ParsedMessage, parse_message};

    #[test]
    fn parses_l2_book_snapshot() {
        let raw = r#"{
            "e": "L2Book",
            "t": 1760000000123,
            "data": {
                "s": "BTCUSD",
                "t": 1760000000100,
                "a": [["60001", "0.25"]],
                "b": [["60000", "1.5"]]
            }
        }"#;

        let ParsedMessage::L2Book(update) = parse_message(raw).unwrap() else {
            panic!("expected L2Book");
        };

        assert_eq!(update.symbol, "BTCUSD");
        assert_eq!(update.exchange_ts_ms, 1_760_000_000_100);
        assert_eq!(update.previous_ts_ms, None);
        assert!(update.is_snapshot);
        assert_eq!(update.bids[0].level.price.to_string(), "60000");
        assert_eq!(update.bids[0].level.size.to_string(), "1.5");
        assert_eq!(update.asks[0].level.price.to_string(), "60001");
    }

    #[test]
    fn parses_l2_book_delta_with_numeric_levels() {
        let raw = r#"{
            "e": "L2Book",
            "data": {
                "s": "ETHUSD",
                "t": 1760000000200,
                "pt": 1760000000100,
                "a": [[3001.2, 0]],
                "b": [[3000.1, 4.25]]
            }
        }"#;

        let ParsedMessage::L2Book(update) = parse_message(raw).unwrap() else {
            panic!("expected L2Book");
        };

        assert_eq!(update.symbol, "ETHUSD");
        assert_eq!(update.previous_ts_ms, Some(1_760_000_000_100));
        assert!(!update.is_snapshot);
        assert_eq!(update.asks[0].level.size.to_string(), "0");
        assert_eq!(update.bids[0].level.price.to_string(), "3000.1");
    }

    #[test]
    fn ignores_subscription_ack() {
        let raw = r#"{"event":"subscribed","data":{"type":"L2Book","symbol":"BTCUSD"}}"#;
        assert_eq!(parse_message(raw).unwrap(), ParsedMessage::Ignore);
    }
}
