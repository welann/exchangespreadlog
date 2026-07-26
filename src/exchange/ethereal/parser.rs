use std::str::FromStr;

use anyhow::{Result, anyhow, bail};
use serde::Deserialize;
use serde_json::Value;

use crate::domain::{BestLevel, Fixed};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParsedMessage {
    L2Book(L2BookUpdate),
    EngineOpen,
    NamespaceConnected,
    EnginePing,
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
    #[serde(default)]
    ok: Option<bool>,
    #[serde(default)]
    code: Option<String>,
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
    if text == "2" {
        return Ok(ParsedMessage::EnginePing);
    }
    if text.starts_with("0{") {
        return Ok(ParsedMessage::EngineOpen);
    }
    if text.starts_with("40/v1/stream,") {
        return Ok(ParsedMessage::NamespaceConnected);
    }
    if let Some(payload) = text.strip_prefix("42/v1/stream,") {
        return parse_socket_io_event(payload);
    }

    let envelope: Envelope = serde_json::from_str(text)?;

    if envelope.ok == Some(false) {
        bail!(
            "Ethereal subscription rejected: {}",
            envelope.code.unwrap_or_else(|| text.to_string())
        );
    }
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

fn parse_socket_io_event(payload: &str) -> Result<ParsedMessage> {
    let event: Vec<Value> = serde_json::from_str(payload)?;
    let event_name = event
        .first()
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("Ethereal Socket.IO event is missing its name"))?;
    let data = event
        .get(1)
        .cloned()
        .ok_or_else(|| anyhow!("Ethereal Socket.IO event is missing its payload"))?;
    match event_name {
        "BookDepth" => parse_book_depth(data).map(ParsedMessage::L2Book),
        "exception" => bail!("Ethereal Socket.IO exception: {data}"),
        _ => Ok(ParsedMessage::Ignore),
    }
}

#[derive(Debug, Deserialize)]
struct BookDepthData {
    #[serde(rename = "productId")]
    product_id: String,
    timestamp: i64,
    #[serde(default, rename = "previousTimestamp")]
    previous_timestamp: Option<i64>,
    #[serde(default)]
    bids: Vec<[Value; 2]>,
    #[serde(default)]
    asks: Vec<[Value; 2]>,
}

fn parse_book_depth(data: Value) -> Result<L2BookUpdate> {
    let payload: BookDepthData = serde_json::from_value(data)?;
    Ok(L2BookUpdate {
        symbol: payload.product_id,
        exchange_ts_ms: payload.timestamp,
        previous_ts_ms: payload.previous_timestamp,
        is_snapshot: payload.previous_timestamp.is_none(),
        bids: parse_levels(payload.bids)?,
        asks: parse_levels(payload.asks)?,
    })
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

    #[test]
    fn rejects_documented_native_subscription_error() {
        let raw = r#"{"ok":false,"code":"UNKNOWN_PRODUCT"}"#;
        assert!(
            parse_message(raw)
                .unwrap_err()
                .to_string()
                .contains("UNKNOWN_PRODUCT")
        );
    }

    #[test]
    fn parses_mainnet_socket_io_book_depth() {
        let raw = r#"42/v1/stream,["BookDepth",{"productId":"product-1","timestamp":1760000000200,"previousTimestamp":1760000000100,"asks":[["3001","0"]],"bids":[["3000","2"]],"t":1760000000201}]"#;
        let ParsedMessage::L2Book(update) = parse_message(raw).unwrap() else {
            panic!("expected Socket.IO BookDepth");
        };
        assert_eq!(update.symbol, "product-1");
        assert_eq!(update.previous_ts_ms, Some(1_760_000_000_100));
        assert_eq!(update.bids[0].level.size.to_string(), "2");
    }

    #[test]
    fn surfaces_socket_io_exception() {
        let raw = r#"42/v1/stream,["exception",{"pattern":"subscribe","status":"BadRequest"}]"#;
        assert!(parse_message(raw).is_err());
    }
}
