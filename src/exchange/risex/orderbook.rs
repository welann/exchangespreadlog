use std::collections::{BTreeMap, HashMap};

use anyhow::Result;

use crate::domain::{BboTick, BestLevel, Fixed, MarketRef, SourceKind, Venue};

use super::parser::{OrderbookDelta, RiseLevelDelta};

#[derive(Debug, Default)]
pub struct RisexBooks {
    books: HashMap<String, MarketBook>,
}

#[derive(Debug, Default)]
struct MarketBook {
    symbol: Option<String>,
    has_snapshot: bool,
    checksum_compatible: Option<bool>,
    bids: BTreeMap<Fixed, StoredLevel>,
    asks: BTreeMap<Fixed, StoredLevel>,
}

#[derive(Debug, Clone)]
struct StoredLevel {
    level: BestLevel,
    raw_price: String,
    raw_size: String,
}

impl RisexBooks {
    pub fn apply(
        &mut self,
        delta: OrderbookDelta,
        recv_ts_ns: i128,
        configured_symbol: Option<&str>,
    ) -> Result<Option<BboTick>> {
        let market_id = delta.market_id.clone();
        let symbol = configured_symbol
            .map(str::to_string)
            .or_else(|| delta.product.as_deref().map(symbol_from_product));
        let book = self.books.entry(market_id.clone()).or_default();
        if symbol.is_some() {
            book.symbol = symbol;
        }

        if delta.is_snapshot {
            book.bids.clear();
            book.asks.clear();
            book.has_snapshot = true;
            book.checksum_compatible = None;
        } else if !book.has_snapshot {
            return Ok(None);
        }

        apply_side(&mut book.bids, delta.bids);
        apply_side(&mut book.asks, delta.asks);

        let checksum_note = if let (Some(expected), Some(true) | None) =
            (delta.checksum, book.checksum_compatible)
        {
            let actual = book.checksum();
            if actual != expected {
                book.checksum_compatible = Some(false);
                Some(format!(
                    "RiseX checksum could not be verified for market {market_id}: expected {expected}, actual {actual}"
                ))
            } else {
                book.checksum_compatible = Some(true);
                None
            }
        } else {
            None
        };

        let bid = book
            .bids
            .iter()
            .next_back()
            .map(|(_, level)| level.level.clone());
        let ask = book
            .asks
            .iter()
            .next()
            .map(|(_, level)| level.level.clone());

        let mut tick = BboTick::new(
            Venue::Risex,
            MarketRef::new(market_id, book.symbol.clone()),
            recv_ts_ns,
            delta.exchange_ts_ms,
            delta.sequence,
            bid,
            ask,
            SourceKind::L2Book,
        );
        if let Some(note) = checksum_note {
            tick.quality.note = Some(note);
        }

        Ok(Some(tick))
    }
}

fn apply_side(side: &mut BTreeMap<Fixed, StoredLevel>, levels: Vec<RiseLevelDelta>) {
    for level in levels {
        if level.level.size.value() == 0 {
            side.remove(&level.level.price);
        } else {
            side.insert(
                level.level.price,
                StoredLevel {
                    level: level.level,
                    raw_price: level.raw_price,
                    raw_size: level.raw_size,
                },
            );
        }
    }
}

impl MarketBook {
    fn checksum(&self) -> u32 {
        let mut parts = Vec::new();
        let mut bids = self.bids.values().rev();
        let mut asks = self.asks.values();

        loop {
            let bid = bids.next();
            let ask = asks.next();
            if bid.is_none() && ask.is_none() {
                break;
            }
            if let Some(level) = bid {
                parts.push(level.raw_price.as_str());
                parts.push(level.raw_size.as_str());
            }
            if let Some(level) = ask {
                parts.push(level.raw_price.as_str());
                parts.push(level.raw_size.as_str());
            }
        }

        crc32_ieee(parts.join(":").as_bytes())
    }
}

fn crc32_ieee(bytes: &[u8]) -> u32 {
    let mut crc = 0xffff_ffff_u32;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            let mask = 0_u32.wrapping_sub(crc & 1);
            crc = (crc >> 1) ^ (0xedb8_8320 & mask);
        }
    }
    !crc
}

fn symbol_from_product(product: &str) -> String {
    product
        .split_once('-')
        .map(|(base, _)| base.to_string())
        .unwrap_or_else(|| product.to_string())
}

#[cfg(test)]
mod tests {
    use super::{RisexBooks, crc32_ieee};
    use crate::exchange::risex::parser::{ParsedMessage, parse_message};

    #[test]
    fn crc32_matches_standard_check_value() {
        assert_eq!(crc32_ieee(b"123456789"), 0xcbf4_3926);
    }

    #[test]
    fn applies_snapshot_and_updates_top_of_book() {
        let snapshot = r#"{
            "channel":"orderbook",
            "type":"snapshot",
            "market_id":"1",
            "data":{
                "market_id":1,
                "bids":[
                    {"price":"100","quantity":"1","order_count":1},
                    {"price":"99","quantity":"2","order_count":2}
                ],
                "asks":[
                    {"price":"101","quantity":"3","order_count":3},
                    {"price":"102","quantity":"4","order_count":4}
                ]
            },
            "worker_timestamp":"1782670785545813123"
        }"#;
        let update = r#"{
            "channel":"orderbook",
            "type":"update",
            "market_id":"1",
            "data":{
                "market_id":1,
                "bids":[{"price":"100","quantity":"0","order_count":0}],
                "asks":[{"price":"100.5","quantity":"5","order_count":5}]
            },
            "block_number":2,
            "log_index":7
        }"#;

        let mut books = RisexBooks::default();
        let ParsedMessage::Orderbook(snapshot) = parse_message(snapshot).unwrap() else {
            panic!("expected snapshot");
        };
        let tick = books.apply(snapshot, 123, Some("BTC")).unwrap().unwrap();
        assert_eq!(tick.venue.as_str(), "risex");
        assert_eq!(tick.market.symbol.as_deref(), Some("BTC"));
        assert_eq!(tick.bid.unwrap().price.to_string(), "100");
        assert_eq!(tick.ask.unwrap().price.to_string(), "101");

        let ParsedMessage::Orderbook(update) = parse_message(update).unwrap() else {
            panic!("expected update");
        };
        let tick = books.apply(update, 124, Some("BTC")).unwrap().unwrap();
        assert_eq!(tick.sequence, Some(2_000_007));
        assert_eq!(tick.bid.unwrap().price.to_string(), "99");
        assert_eq!(tick.ask.unwrap().price.to_string(), "100.5");
    }

    #[test]
    fn ignores_update_before_snapshot() {
        let update = r#"{
            "channel":"orderbook",
            "type":"update",
            "market_id":"1",
            "data":{
                "market_id":1,
                "bids":[{"price":"100","quantity":"1","order_count":1}],
                "asks":[]
            }
        }"#;

        let ParsedMessage::Orderbook(update) = parse_message(update).unwrap() else {
            panic!("expected update");
        };
        let mut books = RisexBooks::default();
        assert!(books.apply(update, 123, Some("BTC")).unwrap().is_none());
    }

    #[test]
    fn notes_checksum_mismatch_once_without_rejecting_tick() {
        let snapshot = r#"{
            "channel":"orderbook",
            "type":"snapshot",
            "market_id":"1",
            "data":{
                "market_id":1,
                "bids":[{"price":"100","quantity":"1","order_count":1}],
                "asks":[{"price":"101","quantity":"1","order_count":1}]
            },
            "checksum":1
        }"#;

        let ParsedMessage::Orderbook(snapshot) = parse_message(snapshot).unwrap() else {
            panic!("expected snapshot");
        };
        let mut books = RisexBooks::default();
        let tick = books.apply(snapshot, 123, Some("BTC")).unwrap().unwrap();

        assert!(!tick.quality.inconsistent);
        assert!(
            tick.quality
                .note
                .as_deref()
                .unwrap()
                .contains("checksum could not be verified")
        );

        let update = r#"{
            "channel":"orderbook",
            "type":"update",
            "market_id":"1",
            "data":{
                "market_id":1,
                "bids":[{"price":"100","quantity":"1","order_count":1}],
                "asks":[]
            },
            "checksum":2
        }"#;
        let ParsedMessage::Orderbook(update) = parse_message(update).unwrap() else {
            panic!("expected update");
        };
        let tick = books.apply(update, 124, Some("BTC")).unwrap().unwrap();
        assert!(tick.quality.note.is_none());
    }
}
