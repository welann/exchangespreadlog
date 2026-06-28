use std::str::FromStr;

use anyhow::{Result, anyhow};
use serde::Deserialize;
use serde_json::Value;

use crate::domain::{BboTick, BestLevel, Fixed, MarketRef, SourceKind, Venue};

#[derive(Debug, Deserialize)]
struct Envelope {
    channel: String,
    data: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct WsBbo {
    coin: String,
    time: i64,
    bbo: [Option<WsLevel>; 2],
}

#[derive(Debug, Deserialize)]
struct WsBook {
    coin: String,
    levels: [Vec<WsLevel>; 2],
    time: i64,
}

#[derive(Debug, Deserialize)]
struct WsLevel {
    px: String,
    sz: String,
    n: Option<u32>,
}

pub fn parse_message(text: &str, recv_ts_ns: i128) -> Result<Option<BboTick>> {
    let envelope: Envelope = serde_json::from_str(text)?;
    let Some(data) = envelope.data else {
        return Ok(None);
    };

    match envelope.channel.as_str() {
        "bbo" => parse_bbo(data, recv_ts_ns).map(Some),
        "l2Book" => parse_l2_book(data, recv_ts_ns).map(Some),
        "subscriptionResponse" | "pong" => Ok(None),
        _ => Ok(None),
    }
}

fn parse_bbo(data: Value, recv_ts_ns: i128) -> Result<BboTick> {
    let bbo: WsBbo = serde_json::from_value(data)?;
    let bid = bbo.bbo[0].as_ref().map(parse_level).transpose()?;
    let ask = bbo.bbo[1].as_ref().map(parse_level).transpose()?;

    Ok(BboTick::new(
        Venue::Hyperliquid,
        MarketRef::new(bbo.coin.clone(), Some(bbo.coin)),
        recv_ts_ns,
        Some(bbo.time),
        None,
        bid,
        ask,
        SourceKind::Bbo,
    ))
}

fn parse_l2_book(data: Value, recv_ts_ns: i128) -> Result<BboTick> {
    let book: WsBook = serde_json::from_value(data)?;
    let bid = book.levels[0].first().map(parse_level).transpose()?;
    let ask = book.levels[1].first().map(parse_level).transpose()?;

    Ok(BboTick::new(
        Venue::Hyperliquid,
        MarketRef::new(book.coin.clone(), Some(book.coin)),
        recv_ts_ns,
        Some(book.time),
        None,
        bid,
        ask,
        SourceKind::L2Book,
    ))
}

fn parse_level(level: &WsLevel) -> Result<BestLevel> {
    Ok(BestLevel::new(
        Fixed::from_str(&level.px).map_err(|err| anyhow!("invalid Hyperliquid price: {err}"))?,
        Fixed::from_str(&level.sz).map_err(|err| anyhow!("invalid Hyperliquid size: {err}"))?,
        level.n,
    ))
}

#[cfg(test)]
mod tests {
    use super::parse_message;
    use crate::domain::{SourceKind, Venue};

    #[test]
    fn parses_hyperliquid_bbo() {
        let raw = r#"{
            "channel": "bbo",
            "data": {
                "coin": "BTC",
                "time": 1710000000000,
                "bbo": [
                    {"px": "65000.5", "sz": "1.23", "n": 4},
                    {"px": "65001.0", "sz": "0.42", "n": 2}
                ]
            }
        }"#;

        let tick = parse_message(raw, 123).unwrap().unwrap();
        assert_eq!(tick.venue, Venue::Hyperliquid);
        assert_eq!(tick.market.label(), "BTC");
        assert_eq!(tick.source, SourceKind::Bbo);
        assert_eq!(tick.bid.unwrap().price.to_string(), "65000.5");
        assert_eq!(tick.ask.unwrap().size.to_string(), "0.42");
    }

    #[test]
    fn ignores_subscription_ack() {
        let raw = r#"{"channel":"subscriptionResponse","data":{"ok":true}}"#;
        assert!(parse_message(raw, 123).unwrap().is_none());
    }

    #[test]
    fn parses_l2_book_top_level() {
        let raw = r#"{
            "channel": "l2Book",
            "data": {
                "coin": "ETH",
                "time": 1710000000001,
                "levels": [
                    [
                        {"px": "3500.1", "sz": "4.2", "n": 7},
                        {"px": "3500.0", "sz": "1.0", "n": 1}
                    ],
                    [
                        {"px": "3500.2", "sz": "3.1", "n": 5}
                    ]
                ]
            }
        }"#;

        let tick = parse_message(raw, 123).unwrap().unwrap();
        assert_eq!(tick.market.label(), "ETH");
        assert_eq!(tick.source, SourceKind::L2Book);
        assert_eq!(tick.bid.unwrap().price.to_string(), "3500.1");
        assert_eq!(tick.ask.unwrap().order_count, Some(5));
    }
}
