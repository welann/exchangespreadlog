use std::str::FromStr;

use anyhow::{Result, anyhow};
use serde::Deserialize;
use serde_json::Value;

use crate::domain::{BestLevel, Fixed};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParsedMessage {
    Orderbook(OrderbookDelta),
    JsonPing,
    Ignore,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrderbookDelta {
    pub market_id: String,
    pub product: Option<String>,
    pub exchange_ts_ms: Option<i64>,
    pub sequence: Option<i128>,
    pub is_snapshot: bool,
    pub bids: Vec<RiseLevelDelta>,
    pub asks: Vec<RiseLevelDelta>,
    pub checksum: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RiseLevelDelta {
    pub level: BestLevel,
    pub raw_price: String,
    pub raw_size: String,
}

#[derive(Debug, Deserialize)]
struct Envelope {
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    method: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    channel: Option<String>,
    #[serde(default)]
    product: Option<String>,
    #[serde(default)]
    r#type: Option<String>,
    #[serde(default)]
    market_id: Option<String>,
    #[serde(default)]
    data: Option<Value>,
    #[serde(default)]
    checksum: Option<u32>,
    #[serde(default)]
    block_number: Option<u64>,
    #[serde(default)]
    log_index: Option<u64>,
    #[serde(default)]
    timestamp: Option<String>,
    #[serde(default)]
    worker_timestamp: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OrderbookData {
    market_id: Value,
    #[serde(default)]
    bids: Vec<WsLevel>,
    #[serde(default)]
    asks: Vec<WsLevel>,
}

#[derive(Debug, Deserialize)]
struct WsLevel {
    price: String,
    quantity: String,
    #[serde(default)]
    order_count: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct LegacyBookData {
    #[serde(default)]
    bids: Vec<[String; 2]>,
    #[serde(default)]
    asks: Vec<[String; 2]>,
    #[serde(default)]
    timestamp: Option<String>,
}

pub fn parse_message(text: &str) -> Result<ParsedMessage> {
    let envelope: Envelope = serde_json::from_str(text)?;

    if envelope.status.as_deref() == Some("error") || envelope.r#type.as_deref() == Some("error") {
        return Err(anyhow!(
            "RiseX websocket error: {}",
            envelope.message.unwrap_or_else(|| text.to_string())
        ));
    }

    if envelope.r#type.as_deref() == Some("ping") || envelope.method.as_deref() == Some("ping") {
        return Ok(ParsedMessage::JsonPing);
    }

    if envelope.r#type.as_deref() == Some("pong") || envelope.method.as_deref() == Some("pong") {
        return Ok(ParsedMessage::Ignore);
    }

    let channel = envelope.channel.clone();
    let message_type = envelope.r#type.clone();
    let data = envelope.data.clone();

    match (channel.as_deref(), message_type.as_deref(), data) {
        (Some("orderbook"), Some("snapshot" | "update"), Some(data)) => {
            parse_orderbook(envelope, data).map(ParsedMessage::Orderbook)
        }
        (Some("book"), Some("snapshot" | "update"), Some(data)) => {
            parse_legacy_book(envelope, data).map(ParsedMessage::Orderbook)
        }
        _ => Ok(ParsedMessage::Ignore),
    }
}

fn parse_orderbook(envelope: Envelope, data: Value) -> Result<OrderbookDelta> {
    let payload: OrderbookData = serde_json::from_value(data)?;
    let market_id = envelope
        .market_id
        .or_else(|| value_to_string(&payload.market_id))
        .ok_or_else(|| anyhow!("RiseX orderbook message missing market_id"))?;

    Ok(OrderbookDelta {
        market_id,
        product: None,
        exchange_ts_ms: parse_ns_timestamp(
            envelope
                .timestamp
                .as_deref()
                .or(envelope.worker_timestamp.as_deref()),
        ),
        sequence: sequence_from_block(envelope.block_number, envelope.log_index),
        is_snapshot: envelope.r#type.as_deref() == Some("snapshot"),
        bids: parse_ws_levels(payload.bids)?,
        asks: parse_ws_levels(payload.asks)?,
        checksum: envelope.checksum,
    })
}

fn parse_legacy_book(envelope: Envelope, data: Value) -> Result<OrderbookDelta> {
    let payload: LegacyBookData = serde_json::from_value(data)?;
    let product = envelope
        .product
        .ok_or_else(|| anyhow!("RiseX legacy book message missing product"))?;

    Ok(OrderbookDelta {
        market_id: product.clone(),
        product: Some(product),
        exchange_ts_ms: parse_ns_timestamp(
            payload
                .timestamp
                .as_deref()
                .or(envelope.timestamp.as_deref())
                .or(envelope.worker_timestamp.as_deref()),
        ),
        sequence: sequence_from_block(envelope.block_number, envelope.log_index),
        is_snapshot: envelope.r#type.as_deref() == Some("snapshot"),
        bids: parse_legacy_levels(payload.bids)?,
        asks: parse_legacy_levels(payload.asks)?,
        checksum: envelope.checksum,
    })
}

fn parse_ws_levels(levels: Vec<WsLevel>) -> Result<Vec<RiseLevelDelta>> {
    levels
        .into_iter()
        .map(|level| {
            let price = fixed_from_decimal_or_wei(&level.price)
                .map_err(|err| anyhow!("invalid RiseX price: {err}"))?;
            let size = fixed_from_decimal_or_wei(&level.quantity)
                .map_err(|err| anyhow!("invalid RiseX quantity: {err}"))?;
            Ok(RiseLevelDelta {
                level: BestLevel::new(price, size, level.order_count),
                raw_price: level.price,
                raw_size: level.quantity,
            })
        })
        .collect()
}

fn parse_legacy_levels(levels: Vec<[String; 2]>) -> Result<Vec<RiseLevelDelta>> {
    levels
        .into_iter()
        .map(|[price, size]| {
            Ok(RiseLevelDelta {
                level: BestLevel::new(
                    Fixed::from_str(&price)
                        .map_err(|err| anyhow!("invalid RiseX legacy price: {err}"))?,
                    Fixed::from_str(&size)
                        .map_err(|err| anyhow!("invalid RiseX legacy size: {err}"))?,
                    None,
                ),
                raw_price: price,
                raw_size: size,
            })
        })
        .collect()
}

fn fixed_from_decimal_or_wei(input: &str) -> Result<Fixed> {
    let input = input.trim();
    if input.contains('.') || input.len() < 18 || input == "0" {
        return Fixed::from_str(input);
    }

    Fixed::from_str(&decimal_from_wei(input))
}

fn decimal_from_wei(input: &str) -> String {
    let (negative, unsigned) = input
        .strip_prefix('-')
        .map(|value| (true, value))
        .unwrap_or((false, input));
    let sign = if negative { "-" } else { "" };

    if unsigned.len() <= 18 {
        return format!("{sign}0.{:0>18}", unsigned);
    }

    let split = unsigned.len() - 18;
    format!("{sign}{}.{}", &unsigned[..split], &unsigned[split..])
}

fn parse_ns_timestamp(value: Option<&str>) -> Option<i64> {
    let ns = value?.parse::<i128>().ok()?;
    i64::try_from(ns / 1_000_000).ok()
}

fn sequence_from_block(block_number: Option<u64>, log_index: Option<u64>) -> Option<i128> {
    let block = block_number?;
    let log = log_index.unwrap_or_default();
    Some(i128::from(block) * 1_000_000 + i128::from(log))
}

fn value_to_string(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{ParsedMessage, parse_message};

    #[test]
    fn parses_orderbook_snapshot_with_wei_levels() {
        let raw = r#"{
            "method": "snapshot",
            "channel": "orderbook",
            "type": "snapshot",
            "market_id": "1",
            "data": {
                "market_id": 1,
                "bids": [
                    {
                        "price": "50000000000000000000000",
                        "quantity": "1000000000000000000",
                        "order_count": 3,
                        "block_number": 12345678,
                        "log_index": 0
                    }
                ],
                "asks": [
                    {
                        "price": "50100000000000000000000",
                        "quantity": "800000000000000000",
                        "order_count": 2,
                        "block_number": 12345675,
                        "log_index": 1
                    }
                ]
            },
            "level_count": 2,
            "timestamp": "1703123456123456789"
        }"#;

        let ParsedMessage::Orderbook(delta) = parse_message(raw).unwrap() else {
            panic!("expected orderbook snapshot");
        };

        assert!(delta.is_snapshot);
        assert_eq!(delta.market_id, "1");
        assert_eq!(delta.exchange_ts_ms, Some(1_703_123_456_123));
        assert_eq!(delta.bids[0].level.price.to_string(), "50000");
        assert_eq!(delta.bids[0].level.size.to_string(), "1");
        assert_eq!(delta.asks[0].level.price.to_string(), "50100");
        assert_eq!(delta.asks[0].level.size.to_string(), "0.8");
        assert_eq!(delta.asks[0].level.order_count, Some(2));
    }

    #[test]
    fn parses_live_mainnet_orderbook_snapshot_with_decimal_levels() {
        let raw = r#"{
            "block_number":15065028,
            "channel":"orderbook",
            "data":{
                "asks":[{"order_count":1,"price":"59571.9","quantity":"0.000647"}],
                "bids":[{"order_count":1,"price":"59571.8","quantity":"0.121659"}],
                "market_id":1
            },
            "level_count":2,
            "log_index":239,
            "market_id":"1",
            "method":"snapshot",
            "type":"snapshot",
            "worker_timestamp":"1782670785545813123"
        }"#;

        let ParsedMessage::Orderbook(delta) = parse_message(raw).unwrap() else {
            panic!("expected orderbook snapshot");
        };

        assert!(delta.is_snapshot);
        assert_eq!(delta.market_id, "1");
        assert_eq!(delta.exchange_ts_ms, Some(1_782_670_785_545));
        assert_eq!(delta.sequence, Some(15_065_028_000_239));
        assert_eq!(delta.bids[0].level.price.to_string(), "59571.8");
        assert_eq!(delta.bids[0].level.size.to_string(), "0.121659");
        assert_eq!(delta.asks[0].level.price.to_string(), "59571.9");
    }

    #[test]
    fn parses_orderbook_update_deletion() {
        let raw = r#"{
            "channel": "orderbook",
            "type": "update",
            "market_id": "1",
            "data": {
                "market_id": 1,
                "bids": [
                    {
                        "price": "49900000000000000000000",
                        "quantity": "0",
                        "order_count": 0,
                        "block_number": 12345680,
                        "log_index": 2
                    }
                ],
                "asks": []
            },
            "checksum": 1234567890,
            "block_number": 12345680,
            "log_index": 2,
            "timestamp": "1703123458123456789"
        }"#;

        let ParsedMessage::Orderbook(delta) = parse_message(raw).unwrap() else {
            panic!("expected orderbook update");
        };

        assert!(!delta.is_snapshot);
        assert_eq!(delta.checksum, Some(1_234_567_890));
        assert_eq!(delta.sequence, Some(12_345_680_000_002));
        assert_eq!(delta.bids[0].level.price.to_string(), "49900");
        assert_eq!(delta.bids[0].level.size.to_string(), "0");
    }

    #[test]
    fn parses_legacy_book_decimal_levels() {
        let raw = r#"{
            "channel": "book",
            "product": "BTC-PERP",
            "type": "snapshot",
            "data": {
                "bids": [["36000", "50"]],
                "asks": [["36500.5", "10"]],
                "timestamp": "1701798871000000000"
            },
            "timestamp": "1701798871000000000"
        }"#;

        let ParsedMessage::Orderbook(delta) = parse_message(raw).unwrap() else {
            panic!("expected legacy orderbook");
        };

        assert_eq!(delta.market_id, "BTC-PERP");
        assert_eq!(delta.product.as_deref(), Some("BTC-PERP"));
        assert_eq!(delta.bids[0].level.price.to_string(), "36000");
        assert_eq!(delta.asks[0].level.price.to_string(), "36500.5");
    }

    #[test]
    fn ignores_subscription_ack() {
        let raw = r#"{
            "method": "subscribe",
            "status": "success",
            "message": "Subscribed to orderbook",
            "channel": "orderbook"
        }"#;

        assert_eq!(parse_message(raw).unwrap(), ParsedMessage::Ignore);
    }

    #[test]
    fn detects_json_ping() {
        assert_eq!(
            parse_message(r#"{"type":"ping"}"#).unwrap(),
            ParsedMessage::JsonPing
        );
    }
}
