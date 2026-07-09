use std::collections::{BTreeMap, HashMap};

use crate::domain::{BboTick, BestLevel, Fixed, InstrumentRef, SourceKind};

use super::parser::{PerplBookUpdate, PerplLevelDelta};

#[derive(Debug, Default)]
pub(crate) struct PerplBooks {
    books: HashMap<u64, MarketBook>,
}

#[derive(Debug, Default)]
struct MarketBook {
    instrument: Option<InstrumentRef>,
    last_sequence: Option<i128>,
    exchange_ts_ms: Option<i64>,
    bids: BTreeMap<Fixed, BestLevel>,
    asks: BTreeMap<Fixed, BestLevel>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MarketScale {
    pub price_decimals: u32,
    pub size_decimals: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ApplyResult {
    Tick(BboTick),
    Skipped,
}

impl PerplBooks {
    pub(crate) fn apply(
        &mut self,
        update: PerplBookUpdate,
        instrument: InstrumentRef,
        scale: MarketScale,
        recv_ts_ns: i128,
    ) -> ApplyResult {
        let book = self.books.entry(update.sid).or_default();
        book.instrument = Some(instrument.clone());

        if update.is_snapshot {
            book.bids.clear();
            book.asks.clear();
        } else if book.last_sequence.is_none() {
            return ApplyResult::Skipped;
        }

        apply_side(&mut book.bids, update.bids, scale);
        apply_side(&mut book.asks, update.asks, scale);
        book.last_sequence = update.sequence;
        book.exchange_ts_ms = update.exchange_ts_ms;

        ApplyResult::Tick(build_tick(recv_ts_ns, instrument, book))
    }
}

fn apply_side(
    side: &mut BTreeMap<Fixed, BestLevel>,
    levels: Vec<PerplLevelDelta>,
    scale: MarketScale,
) {
    for level in levels {
        let price = Fixed::new(level.price, scale.price_decimals);
        if level.size == 0 {
            side.remove(&price);
        } else {
            side.insert(
                price,
                BestLevel::new(
                    price,
                    Fixed::new(level.size, scale.size_decimals),
                    level.order_count,
                ),
            );
        }
    }
}

fn build_tick(recv_ts_ns: i128, instrument: InstrumentRef, book: &MarketBook) -> BboTick {
    let bid = book.bids.iter().next_back().map(|(_, level)| level.clone());
    let ask = book.asks.iter().next().map(|(_, level)| level.clone());

    BboTick::new(
        instrument,
        recv_ts_ns,
        book.exchange_ts_ms,
        book.last_sequence,
        bid,
        ask,
        SourceKind::L2Book,
    )
}

#[cfg(test)]
mod tests {
    use super::{ApplyResult, MarketScale, PerplBooks};
    use crate::{
        domain::{InstrumentCatalog, ProductType},
        exchange::perpl::parser::{ParsedMessage, parse_message},
    };

    fn instrument() -> crate::domain::InstrumentRef {
        InstrumentCatalog::new(
            "perpl",
            "1",
            "BTC",
            Some("1".to_string()),
            ProductType::Perp,
            "BTC",
            "AUSD",
            "AUSD",
            "AUSD",
            None,
            None,
            None,
            "active",
            None,
        )
        .instrument_ref()
    }

    fn l2(raw: &str) -> crate::exchange::perpl::parser::PerplBookUpdate {
        let ParsedMessage::L2Book(update) = parse_message(raw).unwrap() else {
            panic!("expected L2Book update");
        };
        update
    }

    #[test]
    fn applies_snapshot_and_update_with_scaled_levels() {
        let snapshot = l2(r#"{
            "mt":15,
            "sid":1000001,
            "sn":10,
            "at":{"t":1783610131000},
            "bid":[{"p":630343,"s":44658,"o":3},{"p":630300,"s":100,"o":1}],
            "ask":[{"p":630352,"s":760,"o":5},{"p":630400,"s":100,"o":1}]
        }"#);
        let update = l2(r#"{
            "mt":16,
            "sid":1000001,
            "sn":11,
            "at":{"t":1783610132000},
            "bid":[{"p":630343,"s":0,"o":0}],
            "ask":[{"p":630350,"s":1200,"o":2}]
        }"#);

        let mut books = PerplBooks::default();
        let scale = MarketScale {
            price_decimals: 1,
            size_decimals: 5,
        };

        let ApplyResult::Tick(tick) = books.apply(snapshot, instrument(), scale, 123) else {
            panic!("expected snapshot tick");
        };
        assert_eq!(tick.exchange_ts_ms, Some(1_783_610_131_000));
        assert_eq!(tick.sequence, Some(10));
        assert_eq!(tick.bid.unwrap().price.to_string(), "63034.3");
        assert_eq!(tick.ask.unwrap().size.to_string(), "0.00760");

        let ApplyResult::Tick(tick) = books.apply(update, instrument(), scale, 124) else {
            panic!("expected update tick");
        };
        assert_eq!(tick.sequence, Some(11));
        assert_eq!(tick.bid.unwrap().price.to_string(), "63030.0");
        assert_eq!(tick.ask.unwrap().price.to_string(), "63035.0");
    }

    #[test]
    fn skips_update_before_snapshot() {
        let update = l2(r#"{
            "mt":16,
            "sid":1000001,
            "sn":11,
            "bid":[{"p":630343,"s":0,"o":0}],
            "ask":[]
        }"#);

        let mut books = PerplBooks::default();
        assert_eq!(
            books.apply(
                update,
                instrument(),
                MarketScale {
                    price_decimals: 1,
                    size_decimals: 5
                },
                124
            ),
            ApplyResult::Skipped
        );
    }
}
