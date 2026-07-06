use std::collections::{BTreeMap, HashMap};

use crate::domain::{BboTick, BestLevel, Fixed, MarketRef, SourceKind, Venue};

use super::parser::{OrderbookDelta, OrderbookSnapshot};

#[derive(Debug, Default)]
pub struct ZeroOneBooks {
    books: HashMap<String, MarketBook>,
}

#[derive(Debug, Default)]
struct MarketBook {
    market_id: String,
    label: String,
    update_id: Option<i128>,
    bids: BTreeMap<Fixed, BestLevel>,
    asks: BTreeMap<Fixed, BestLevel>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApplyResult {
    Tick(BboTick),
    Gap {
        market_symbol: String,
        expected_last_update_id: i128,
        received_last_update_id: i128,
    },
    Skipped,
}

impl ZeroOneBooks {
    pub fn apply_snapshot(
        &mut self,
        market_symbol: &str,
        market_id: &str,
        label: &str,
        snapshot: OrderbookSnapshot,
        recv_ts_ns: i128,
    ) -> BboTick {
        let book = self.books.entry(market_symbol.to_string()).or_default();
        book.market_id = market_id.to_string();
        book.label = label.to_string();
        book.update_id = Some(snapshot.update_id);
        book.bids.clear();
        book.asks.clear();
        replace_side(&mut book.bids, snapshot.bids);
        replace_side(&mut book.asks, snapshot.asks);
        build_tick(
            market_id,
            Some(label),
            recv_ts_ns,
            Some(snapshot.update_id),
            book,
        )
    }

    pub fn apply_delta(&mut self, delta: OrderbookDelta, recv_ts_ns: i128) -> ApplyResult {
        let Some(book) = self.books.get_mut(&delta.market_symbol) else {
            return ApplyResult::Gap {
                market_symbol: delta.market_symbol,
                expected_last_update_id: 0,
                received_last_update_id: delta.last_update_id,
            };
        };

        let Some(current_update_id) = book.update_id else {
            return ApplyResult::Gap {
                market_symbol: delta.market_symbol,
                expected_last_update_id: 0,
                received_last_update_id: delta.last_update_id,
            };
        };

        if delta.update_id <= current_update_id {
            return ApplyResult::Skipped;
        }

        if delta.last_update_id > current_update_id {
            return ApplyResult::Gap {
                market_symbol: delta.market_symbol,
                expected_last_update_id: current_update_id,
                received_last_update_id: delta.last_update_id,
            };
        }

        apply_side(&mut book.bids, delta.bids);
        apply_side(&mut book.asks, delta.asks);
        book.update_id = Some(delta.update_id);

        ApplyResult::Tick(build_tick(
            &book.market_id,
            Some(&book.label),
            recv_ts_ns,
            Some(delta.update_id),
            book,
        ))
    }
}

fn replace_side(side: &mut BTreeMap<Fixed, BestLevel>, levels: Vec<BestLevel>) {
    side.extend(levels.into_iter().map(|level| (level.price, level)));
}

fn apply_side(side: &mut BTreeMap<Fixed, BestLevel>, levels: Vec<BestLevel>) {
    for level in levels {
        if level.size.value() == 0 {
            side.remove(&level.price);
        } else {
            side.insert(level.price, level);
        }
    }
}

fn build_tick(
    market_id: &str,
    label: Option<&str>,
    recv_ts_ns: i128,
    sequence: Option<i128>,
    book: &MarketBook,
) -> BboTick {
    let bid = book.bids.iter().next_back().map(|(_, level)| level.clone());
    let ask = book.asks.iter().next().map(|(_, level)| level.clone());

    BboTick::new(
        Venue::ZeroOne,
        MarketRef::new(market_id, label.map(str::to_string)),
        recv_ts_ns,
        None,
        sequence,
        bid,
        ask,
        SourceKind::L2Book,
    )
}

#[cfg(test)]
mod tests {
    use super::{ApplyResult, ZeroOneBooks};
    use crate::exchange::zero_one::parser::{parse_delta, parse_snapshot};

    #[test]
    fn applies_snapshot_and_contiguous_delta() {
        let snapshot = parse_snapshot(
            r#"{
                "updateId": 10,
                "asks": [[101.0, 1.0], [102.0, 2.0]],
                "bids": [[100.0, 1.0], [99.0, 2.0]]
            }"#,
        )
        .unwrap();
        let delta = parse_delta(
            r#"{
                "delta": {
                    "last_update_id": 10,
                    "update_id": 11,
                    "market_symbol": "BTCUSD",
                    "asks": [[100.5, 3.0]],
                    "bids": [[100.0, 0.0]]
                }
            }"#,
        )
        .unwrap()
        .unwrap();

        let mut books = ZeroOneBooks::default();
        let tick = books.apply_snapshot("BTCUSD", "0", "BTC", snapshot, 123);
        assert_eq!(tick.venue.as_str(), "01");
        assert_eq!(tick.market.symbol.as_deref(), Some("BTC"));
        assert_eq!(tick.bid.unwrap().price.to_string(), "100");
        assert_eq!(tick.ask.unwrap().price.to_string(), "101");

        let ApplyResult::Tick(tick) = books.apply_delta(delta, 124) else {
            panic!("expected tick");
        };
        assert_eq!(tick.sequence, Some(11));
        assert_eq!(tick.bid.unwrap().price.to_string(), "99");
        assert_eq!(tick.ask.unwrap().price.to_string(), "100.5");
    }

    #[test]
    fn detects_delta_gap() {
        let snapshot = parse_snapshot(r#"{"updateId":10,"asks":[],"bids":[]}"#).unwrap();
        let delta = parse_delta(
            r#"{
                "delta": {
                    "last_update_id": 12,
                    "update_id": 13,
                    "market_symbol": "BTCUSD",
                    "asks": [],
                    "bids": []
                }
            }"#,
        )
        .unwrap()
        .unwrap();

        let mut books = ZeroOneBooks::default();
        books.apply_snapshot("BTCUSD", "0", "BTC", snapshot, 123);

        assert_eq!(
            books.apply_delta(delta, 124),
            ApplyResult::Gap {
                market_symbol: "BTCUSD".to_string(),
                expected_last_update_id: 10,
                received_last_update_id: 12,
            }
        );
    }

    #[test]
    fn applies_delta_when_snapshot_is_inside_update_range() {
        let snapshot = parse_snapshot(
            r#"{
                "updateId": 12,
                "asks": [[101.0, 1.0]],
                "bids": [[100.0, 1.0]]
            }"#,
        )
        .unwrap();
        let delta = parse_delta(
            r#"{
                "delta": {
                    "last_update_id": 10,
                    "update_id": 13,
                    "market_symbol": "BTCUSD",
                    "asks": [[100.5, 2.0]],
                    "bids": []
                }
            }"#,
        )
        .unwrap()
        .unwrap();

        let mut books = ZeroOneBooks::default();
        books.apply_snapshot("BTCUSD", "0", "BTC", snapshot, 123);

        let ApplyResult::Tick(tick) = books.apply_delta(delta, 124) else {
            panic!("expected overlapping delta to apply");
        };
        assert_eq!(tick.sequence, Some(13));
        assert_eq!(tick.ask.unwrap().price.to_string(), "100.5");
    }

    #[test]
    fn skips_old_delta() {
        let snapshot = parse_snapshot(r#"{"updateId":10,"asks":[],"bids":[]}"#).unwrap();
        let delta = parse_delta(
            r#"{
                "delta": {
                    "last_update_id": 9,
                    "update_id": 10,
                    "market_symbol": "BTCUSD",
                    "asks": [],
                    "bids": []
                }
            }"#,
        )
        .unwrap()
        .unwrap();

        let mut books = ZeroOneBooks::default();
        books.apply_snapshot("BTCUSD", "0", "BTC", snapshot, 123);

        assert_eq!(books.apply_delta(delta, 124), ApplyResult::Skipped);
    }
}
