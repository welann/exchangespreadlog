use std::collections::HashMap;

use crate::domain::BboTick;

#[derive(Debug, Default)]
pub struct DedupeFilter {
    last_by_market: HashMap<String, BboTick>,
}

impl DedupeFilter {
    pub fn should_emit(&mut self, tick: &BboTick) -> bool {
        let key = tick.instrument.catalog_id.clone();
        match self.last_by_market.get(&key) {
            Some(last) if same_observable_bbo(last, tick) => false,
            _ => {
                self.last_by_market.insert(key, tick.clone());
                true
            }
        }
    }

    pub fn reset_venue(&mut self, venue_instance_id: &str) {
        self.last_by_market
            .retain(|_, tick| tick.instrument.venue_instance_id != venue_instance_id);
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
        domain::{BboTick, BestLevel, Fixed, InstrumentCatalog, ProductType, SourceKind},
        pipeline::dedupe::DedupeFilter,
    };

    fn catalog() -> InstrumentCatalog {
        InstrumentCatalog::new(
            "lighter",
            "0",
            "ETH",
            Some("0".to_string()),
            ProductType::Perp,
            "ETH",
            "USDC",
            "USDC",
            "USDC",
            None,
            None,
            None,
            "active",
            None,
        )
    }

    fn tick(sequence: Option<i128>, bid_size: &str) -> BboTick {
        let catalog = catalog();
        BboTick::new(
            catalog.instrument_ref(),
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

    #[test]
    fn reset_venue_allows_the_same_tick_after_resubscription() {
        let mut filter = DedupeFilter::default();
        let first = tick(Some(1), "1");
        assert!(filter.should_emit(&first));
        assert!(!filter.should_emit(&first));

        filter.reset_venue("lighter");

        assert!(filter.should_emit(&first));
    }
}
