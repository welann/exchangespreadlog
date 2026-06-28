use std::str::FromStr;

use anyhow::{Result, anyhow};
use serde::Deserialize;
use serde_json::Value;

use crate::domain::{BboTick, BestLevel, Fixed, MarketRef, SourceKind, Venue};

#[derive(Debug, Deserialize)]
struct TickerEnvelope {
    channel: String,
    #[serde(default)]
    nonce: Option<i128>,
    #[serde(default)]
    timestamp: Option<i64>,
    ticker: Option<Ticker>,
}

#[derive(Debug, Deserialize)]
struct Ticker {
    s: Option<String>,
    a: Option<LighterLevel>,
    b: Option<LighterLevel>,
    #[serde(default)]
    last_updated_at: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct LighterLevel {
    price: String,
    size: String,
}

pub fn parse_message(text: &str, recv_ts_ns: i128) -> Result<Option<BboTick>> {
    let value: Value = serde_json::from_str(text)?;
    if value.get("type").and_then(Value::as_str) != Some("update/ticker") {
        return Ok(None);
    }

    let envelope: TickerEnvelope = serde_json::from_str(text)?;

    let Some(ticker) = envelope.ticker else {
        return Ok(None);
    };

    let market_id = envelope
        .channel
        .split_once(':')
        .map(|(_, id)| id.to_string())
        .unwrap_or_else(|| envelope.channel.clone());
    let symbol = ticker.s.clone();
    let bid = ticker.b.as_ref().map(parse_level).transpose()?;
    let ask = ticker.a.as_ref().map(parse_level).transpose()?;
    let exchange_ts_ms = envelope
        .timestamp
        .or_else(|| ticker.last_updated_at.map(micros_to_millis));

    Ok(Some(BboTick::new(
        Venue::Lighter,
        MarketRef::new(market_id, symbol),
        recv_ts_ns,
        exchange_ts_ms,
        envelope.nonce,
        bid,
        ask,
        SourceKind::Ticker,
    )))
}

fn micros_to_millis(value: i64) -> i64 {
    if value > 10_000_000_000_000 {
        value / 1_000
    } else {
        value
    }
}

fn parse_level(level: &LighterLevel) -> Result<BestLevel> {
    Ok(BestLevel::new(
        Fixed::from_str(&level.price).map_err(|err| anyhow!("invalid Lighter price: {err}"))?,
        Fixed::from_str(&level.size).map_err(|err| anyhow!("invalid Lighter size: {err}"))?,
        None,
    ))
}

#[cfg(test)]
mod tests {
    use super::parse_message;
    use crate::domain::{SourceKind, Venue};

    #[test]
    fn parses_lighter_ticker() {
        let raw = r#"{
            "channel":"ticker:0",
            "last_updated_at":1774883844921166,
            "nonce":9182249734,
            "ticker":{
                "s":"ETH",
                "a":{"price":"2064.48","size":"0.4950"},
                "b":{"price":"2064.30","size":"1.0392"},
                "last_updated_at":1774883844921166
            },
            "timestamp":1774883844933,
            "type":"update/ticker"
        }"#;

        let tick = parse_message(raw, 123).unwrap().unwrap();
        assert_eq!(tick.venue, Venue::Lighter);
        assert_eq!(tick.market.id, "0");
        assert_eq!(tick.market.symbol.as_deref(), Some("ETH"));
        assert_eq!(tick.sequence, Some(9182249734));
        assert_eq!(tick.source, SourceKind::Ticker);
        assert_eq!(tick.bid.unwrap().size.to_string(), "1.0392");
        assert_eq!(tick.ask.unwrap().price.to_string(), "2064.48");
    }

    #[test]
    fn ignores_non_ticker_messages() {
        let raw = r#"{"type":"subscribed/ticker","channel":"ticker:0"}"#;
        assert!(parse_message(raw, 123).unwrap().is_none());
    }

    #[test]
    fn ignores_non_ticker_messages_without_channel() {
        let raw = r#"{"type":"connected","timestamp":1774883844933,"code":200}"#;
        assert!(parse_message(raw, 123).unwrap().is_none());
    }
}
