use std::collections::HashMap;

use crate::domain::BboTick;

#[derive(Debug, Default)]
pub struct DedupeFilter {
    last_by_market: HashMap<String, BboTick>,
}

impl DedupeFilter {
    pub fn should_emit(&mut self, tick: &BboTick) -> bool {
        let key = format!("{}:{}", tick.venue.as_str(), tick.market.id);
        match self.last_by_market.get(&key) {
            Some(last) if same_observable_bbo(last, tick) => false,
            _ => {
                self.last_by_market.insert(key, tick.clone());
                true
            }
        }
    }
}

fn same_observable_bbo(lhs: &BboTick, rhs: &BboTick) -> bool {
    lhs.bid == rhs.bid
        && lhs.ask == rhs.ask
        && lhs.exchange_ts_ms == rhs.exchange_ts_ms
        && lhs.sequence == rhs.sequence
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use crate::{
        domain::{BboTick, BestLevel, Fixed, MarketRef, SourceKind, Venue},
        pipeline::dedupe::DedupeFilter,
    };

    fn tick(sequence: Option<i128>, bid_size: &str) -> BboTick {
        BboTick::new(
            Venue::Lighter,
            MarketRef::new("0", Some("ETH".to_string())),
            123,
            Some(456),
            sequence,
            Some(BestLevel::new(
                Fixed::from_str("100").unwrap(),
                Fixed::from_str(bid_size).unwrap(),
                None,
            )),
            Some(BestLevel::new(
                Fixed::from_str("101").unwrap(),
                Fixed::from_str("2").unwrap(),
                None,
            )),
            SourceKind::Ticker,
        )
    }

    #[test]
    fn suppresses_identical_ticks() {
        let mut filter = DedupeFilter::default();
        let first = tick(Some(1), "1");
        let second = tick(Some(1), "1");

        assert!(filter.should_emit(&first));
        assert!(!filter.should_emit(&second));
    }

    #[test]
    fn emits_when_sequence_or_bbo_changes() {
        let mut filter = DedupeFilter::default();
        assert!(filter.should_emit(&tick(Some(1), "1")));
        assert!(filter.should_emit(&tick(Some(2), "1")));
        assert!(filter.should_emit(&tick(Some(2), "1.1")));
    }
}
