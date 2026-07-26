use std::str::FromStr;

use anyhow::{Result, anyhow, bail};
use chrono::DateTime;
use serde::Deserialize;
use serde_json::Value;

use crate::domain::{BboTick, BestLevel, Fixed, InstrumentRef, SourceKind};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParsedMessage {
    Ticks(Vec<BboTick>),
    Ignore,
}

#[derive(Debug, Deserialize)]
struct Envelope {
    #[serde(default, rename = "type")]
    message_type: Option<String>,
    #[serde(default)]
    channel: Option<String>,
    #[serde(default)]
    timestamp: Option<String>,
    #[serde(default)]
    data: Option<Value>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    message: Option<String>,
}

#[derive(Debug, Deserialize)]
struct BookSnapshot {
    market: String,
    #[serde(default)]
    time: Option<String>,
    #[serde(default)]
    asks: Vec<[Value; 2]>,
    #[serde(default)]
    bids: Vec<[Value; 2]>,
}

pub fn parse_message(
    text: &str,
    recv_ts_ns: i128,
    venue_instance_id: &str,
) -> Result<ParsedMessage> {
    let envelope: Envelope = serde_json::from_str(text)?;
    match envelope.message_type.as_deref() {
        Some("update") if is_book_channel(envelope.channel.as_deref()) => {
            let data = envelope
                .data
                .ok_or_else(|| anyhow!("Ondo book update missing data"))?;
            let snapshots: Vec<BookSnapshot> = serde_json::from_value(data)?;
            snapshots
                .into_iter()
                .map(|snapshot| {
                    parse_book_snapshot(
                        snapshot,
                        envelope.timestamp.as_deref(),
                        recv_ts_ns,
                        venue_instance_id,
                    )
                })
                .collect::<Result<Vec<_>>>()
                .map(ParsedMessage::Ticks)
        }
        Some("error") => {
            let reason = envelope
                .error
                .or(envelope.message)
                .unwrap_or_else(|| text.to_string());
            bail!("Ondo websocket error: {reason}");
        }
        _ => Ok(ParsedMessage::Ignore),
    }
}

fn is_book_channel(channel: Option<&str>) -> bool {
    matches!(channel, Some("topOfBooksPerps" | "depthBooksPerps"))
}

fn parse_book_snapshot(
    snapshot: BookSnapshot,
    fallback_timestamp: Option<&str>,
    recv_ts_ns: i128,
    venue_instance_id: &str,
) -> Result<BboTick> {
    let exchange_ts_ms = snapshot
        .time
        .as_deref()
        .or(fallback_timestamp)
        .map(parse_rfc3339_ms)
        .transpose()?;

    Ok(BboTick::new(
        InstrumentRef::unchecked(venue_instance_id, snapshot.market),
        recv_ts_ns,
        exchange_ts_ms,
        None,
        snapshot.bids.first().map(parse_level).transpose()?,
        snapshot.asks.first().map(parse_level).transpose()?,
        SourceKind::Bbo,
    ))
}

fn parse_level(level: &[Value; 2]) -> Result<BestLevel> {
    let raw_price = decimal_value_to_string(&level[0])?;
    let raw_size = decimal_value_to_string(&level[1])?;
    Ok(BestLevel::new(
        Fixed::from_str(&raw_price).map_err(|err| anyhow!("invalid Ondo price: {err}"))?,
        Fixed::from_str(&raw_size).map_err(|err| anyhow!("invalid Ondo size: {err}"))?,
        None,
    ))
}

fn decimal_value_to_string(value: &Value) -> Result<String> {
    match value {
        Value::String(value) if !value.trim().is_empty() => Ok(value.clone()),
        Value::Number(value) => Ok(value.to_string()),
        other => Err(anyhow!("expected decimal string or number, got {other}")),
    }
}

fn parse_rfc3339_ms(value: &str) -> Result<i64> {
    Ok(DateTime::parse_from_rfc3339(value)?.timestamp_millis())
}

#[cfg(test)]
mod tests {
    use super::{ParsedMessage, parse_message};
    use crate::domain::SourceKind;

    #[test]
    fn parses_top_of_book_update() {
        let raw = r#"{
            "type":"update",
            "channel":"topOfBooksPerps",
            "timestamp":"2026-07-10T16:30:00.710521672Z",
            "data":[{
                "market":"BTC-USD.P",
                "time":"2026-07-10T16:29:55.866545826Z",
                "asks":[["63944.21","0.156"]],
                "bids":[["63940.6","0.0076"]]
            }]
        }"#;

        let ParsedMessage::Ticks(ticks) = parse_message(raw, 123, "ondo").unwrap() else {
            panic!("expected ticks");
        };

        assert_eq!(ticks.len(), 1);
        let tick = &ticks[0];
        assert_eq!(tick.instrument.venue_instance_id, "ondo");
        assert_eq!(tick.instrument.instrument_id, "BTC-USD.P");
        assert_eq!(tick.exchange_ts_ms, Some(1_783_700_995_866));
        assert_eq!(tick.source, SourceKind::Bbo);
        assert_eq!(tick.bid.as_ref().unwrap().price.to_string(), "63940.6");
        assert_eq!(tick.bid.as_ref().unwrap().size.to_string(), "0.0076");
        assert_eq!(tick.ask.as_ref().unwrap().price.to_string(), "63944.21");
    }

    #[test]
    fn parses_numeric_depth_book_levels_using_first_level() {
        let raw = r#"{
            "type":"update",
            "channel":"depthBooksPerps",
            "timestamp":"2026-07-10T16:30:00.710521672Z",
            "data":[{
                "market":"ETH-USD.P",
                "asks":[[3001.2,1.5],[3001.3,2.0]],
                "bids":[[3000.9,0.25],[3000.8,3.0]]
            }]
        }"#;

        let ParsedMessage::Ticks(ticks) = parse_message(raw, 123, "ondo").unwrap() else {
            panic!("expected ticks");
        };

        assert_eq!(ticks[0].instrument.instrument_id, "ETH-USD.P");
        assert_eq!(ticks[0].exchange_ts_ms, Some(1_783_701_000_710));
        assert_eq!(ticks[0].bid.as_ref().unwrap().price.to_string(), "3000.9");
        assert_eq!(ticks[0].ask.as_ref().unwrap().size.to_string(), "1.5");
    }

    #[test]
    fn ignores_subscription_ack_and_pong() {
        let ack = r#"{"type":"subscribed","channel":"topOfBooksPerps","data":{"op":"subscribe"}}"#;
        assert_eq!(
            parse_message(ack, 123, "ondo").unwrap(),
            ParsedMessage::Ignore
        );

        let pong = r#"{"type":"pong"}"#;
        assert_eq!(
            parse_message(pong, 123, "ondo").unwrap(),
            ParsedMessage::Ignore
        );
    }

    #[test]
    fn reports_server_error() {
        let raw = r#"{"type":"error","error":"provided market is invalid"}"#;
        assert!(parse_message(raw, 123, "ondo").is_err());
    }
}
