use std::collections::{BTreeMap, HashMap};

use crate::domain::{BboTick, BestLevel, Fixed, InstrumentRef, SourceKind};

use super::parser::{EtherealLevelDelta, L2BookUpdate};

#[derive(Debug, Default)]
pub struct EtherealBooks {
    books: HashMap<String, MarketBook>,
}

#[derive(Debug, Default)]
struct MarketBook {
    timestamp_ms: Option<i64>,
    instrument: Option<InstrumentRef>,
    bids: BTreeMap<Fixed, BestLevel>,
    asks: BTreeMap<Fixed, BestLevel>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApplyResult {
    Tick(BboTick),
    Gap {
        symbol: String,
        expected_previous_ts_ms: i64,
        received_previous_ts_ms: Option<i64>,
        received_ts_ms: i64,
    },
    Skipped,
}

impl EtherealBooks {
    pub fn apply(
        &mut self,
        update: L2BookUpdate,
        instrument: InstrumentRef,
        recv_ts_ns: i128,
    ) -> ApplyResult {
        let book = self.books.entry(update.symbol.clone()).or_default();

        if update.is_snapshot || book.timestamp_ms.is_none() {
            book.instrument = Some(instrument.clone());
            book.timestamp_ms = Some(update.exchange_ts_ms);
            book.bids.clear();
            book.asks.clear();
            replace_side(&mut book.bids, update.bids);
            replace_side(&mut book.asks, update.asks);
            return ApplyResult::Tick(build_tick(recv_ts_ns, instrument, book));
        }

        let current_ts_ms = book
            .timestamp_ms
            .expect("missing Ethereal book timestamp initializes as snapshot");

        if update.exchange_ts_ms <= current_ts_ms {
            return ApplyResult::Skipped;
        }

        if update.previous_ts_ms != Some(current_ts_ms) {
            return ApplyResult::Gap {
                symbol: update.symbol,
                expected_previous_ts_ms: current_ts_ms,
                received_previous_ts_ms: update.previous_ts_ms,
                received_ts_ms: update.exchange_ts_ms,
            };
        }

        book.instrument = Some(instrument.clone());
        apply_side(&mut book.bids, update.bids);
        apply_side(&mut book.asks, update.asks);
        book.timestamp_ms = Some(update.exchange_ts_ms);

        ApplyResult::Tick(build_tick(recv_ts_ns, instrument, book))
    }
}

fn replace_side(side: &mut BTreeMap<Fixed, BestLevel>, levels: Vec<EtherealLevelDelta>) {
    side.extend(levels.into_iter().filter_map(|delta| {
        if delta.level.size.value() == 0 {
            None
        } else {
            Some((delta.level.price, delta.level))
        }
    }));
}

fn apply_side(side: &mut BTreeMap<Fixed, BestLevel>, levels: Vec<EtherealLevelDelta>) {
    for delta in levels {
        if delta.level.size.value() == 0 {
            side.remove(&delta.level.price);
        } else {
            side.insert(delta.level.price, delta.level);
        }
    }
}

fn build_tick(recv_ts_ns: i128, instrument: InstrumentRef, book: &MarketBook) -> BboTick {
    let bid = book.bids.iter().next_back().map(|(_, level)| level.clone());
    let ask = book.asks.iter().next().map(|(_, level)| level.clone());
    let sequence = book.timestamp_ms.map(i128::from);

    BboTick::new(
        instrument,
        recv_ts_ns,
        book.timestamp_ms,
        sequence,
        bid,
        ask,
        SourceKind::L2Book,
    )
}

#[cfg(test)]
mod tests {
    use super::{ApplyResult, EtherealBooks};
    use crate::{
        domain::{InstrumentCatalog, ProductType},
        exchange::ethereal::parser::{ParsedMessage, parse_message},
    };

    fn instrument() -> crate::domain::InstrumentRef {
        InstrumentCatalog::new(
            "ethereal",
            "BTCUSD",
            "BTC-USD",
            Some("BTCUSD".to_string()),
            ProductType::Perp,
            "BTC",
            "USD",
            "USD",
            "USD",
            None,
            None,
            None,
            "active",
            None,
        )
        .instrument_ref()
    }

    fn l2(raw: &str) -> crate::exchange::ethereal::parser::L2BookUpdate {
        let ParsedMessage::L2Book(update) = parse_message(raw).unwrap() else {
            panic!("expected L2Book update");
        };
        update
    }

    #[test]
    fn applies_snapshot_and_contiguous_delta() {
        let snapshot = l2(r#"{
                "e":"L2Book",
                "data":{
                    "s":"BTCUSD",
                    "t":1000,
                    "a":[["101","1"],["102","2"]],
                    "b":[["100","1"],["99","2"]]
                }
            }"#);
        let delta = l2(r#"{
                "e":"L2Book",
                "data":{
                    "s":"BTCUSD",
                    "t":1010,
                    "pt":1000,
                    "a":[["100.5","3"]],
                    "b":[["100","0"]]
                }
            }"#);

        let mut books = EtherealBooks::default();
        let ApplyResult::Tick(tick) = books.apply(snapshot, instrument(), 123) else {
            panic!("expected snapshot tick");
        };
        assert_eq!(tick.exchange_ts_ms, Some(1000));
        assert_eq!(tick.sequence, Some(1000));
        assert_eq!(tick.bid.unwrap().price.to_string(), "100");
        assert_eq!(tick.ask.unwrap().price.to_string(), "101");

        let ApplyResult::Tick(tick) = books.apply(delta, instrument(), 124) else {
            panic!("expected delta tick");
        };
        assert_eq!(tick.exchange_ts_ms, Some(1010));
        assert_eq!(tick.bid.unwrap().price.to_string(), "99");
        assert_eq!(tick.ask.unwrap().price.to_string(), "100.5");
    }

    #[test]
    fn detects_delta_gap() {
        let snapshot = l2(r#"{"e":"L2Book","data":{"s":"BTCUSD","t":1000,"a":[],"b":[]}}"#);
        let delta = l2(r#"{"e":"L2Book","data":{"s":"BTCUSD","t":1030,"pt":1020,"a":[],"b":[]}}"#);

        let mut books = EtherealBooks::default();
        books.apply(snapshot, instrument(), 123);

        assert_eq!(
            books.apply(delta, instrument(), 124),
            ApplyResult::Gap {
                symbol: "BTCUSD".to_string(),
                expected_previous_ts_ms: 1000,
                received_previous_ts_ms: Some(1020),
                received_ts_ms: 1030,
            }
        );
    }

    #[test]
    fn initializes_first_message_as_snapshot_even_when_pt_is_present() {
        let first = l2(r#"{
                "e":"L2Book",
                "data":{
                    "s":"BTCUSD",
                    "t":1010,
                    "pt":1000,
                    "a":[["101","1"]],
                    "b":[["100","1"]]
                }
            }"#);

        let mut books = EtherealBooks::default();
        let ApplyResult::Tick(tick) = books.apply(first, instrument(), 123) else {
            panic!("expected first message to initialize the book");
        };

        assert_eq!(tick.exchange_ts_ms, Some(1010));
        assert_eq!(tick.bid.unwrap().price.to_string(), "100");
        assert_eq!(tick.ask.unwrap().price.to_string(), "101");
    }

    #[test]
    fn skips_old_delta() {
        let snapshot = l2(r#"{"e":"L2Book","data":{"s":"BTCUSD","t":1000,"a":[],"b":[]}}"#);
        let delta = l2(r#"{"e":"L2Book","data":{"s":"BTCUSD","t":1000,"pt":990,"a":[],"b":[]}}"#);

        let mut books = EtherealBooks::default();
        books.apply(snapshot, instrument(), 123);

        assert_eq!(books.apply(delta, instrument(), 124), ApplyResult::Skipped);
    }
}
