use std::{
    collections::{BTreeSet, HashMap},
    sync::{Arc, RwLock},
};

use crate::domain::{BboTick, Venue};

pub type SharedBboState = Arc<RwLock<BboStore>>;

pub fn new_shared_state() -> SharedBboState {
    Arc::new(RwLock::new(BboStore::default()))
}

#[derive(Debug, Default)]
pub struct BboStore {
    ticks: HashMap<(Venue, String), BboTick>,
}

#[derive(Debug, Clone, Default)]
pub struct BboSnapshot {
    pub ticks: Vec<BboTick>,
    pub venues: Vec<Venue>,
    pub markets: Vec<String>,
}

impl BboStore {
    pub fn update(&mut self, tick: BboTick) {
        let market = market_key(&tick);
        self.ticks.insert((tick.venue, market), tick);
    }

    pub fn snapshot(&self) -> BboSnapshot {
        let mut ticks = self.ticks.values().cloned().collect::<Vec<_>>();
        ticks.sort_by(|lhs, rhs| {
            lhs.market
                .label()
                .cmp(rhs.market.label())
                .then(lhs.venue.as_str().cmp(rhs.venue.as_str()))
        });

        let venues = ticks
            .iter()
            .map(|tick| tick.venue)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let markets = ticks
            .iter()
            .map(market_key)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();

        BboSnapshot {
            ticks,
            venues,
            markets,
        }
    }
}

impl BboSnapshot {
    pub fn find(&self, venue: Venue, market: &str) -> Option<&BboTick> {
        self.ticks
            .iter()
            .find(|tick| tick.venue == venue && market_key(tick) == market)
    }

    pub fn rows_for_market(&self, market: &str) -> Vec<&BboTick> {
        self.ticks
            .iter()
            .filter(|tick| market_key(tick) == market)
            .collect()
    }
}

pub fn market_key(tick: &BboTick) -> String {
    tick.market
        .symbol
        .clone()
        .unwrap_or_else(|| tick.market.id.clone())
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use crate::{
        domain::{BboTick, BestLevel, Fixed, MarketRef, SourceKind, Venue},
        state::{BboStore, market_key},
    };

    fn tick(venue: Venue, market_id: &str, symbol: Option<&str>) -> BboTick {
        BboTick::new(
            venue,
            MarketRef::new(market_id, symbol.map(str::to_string)),
            123,
            Some(456),
            None,
            Some(BestLevel::new(
                Fixed::from_str("100").unwrap(),
                Fixed::from_str("1").unwrap(),
                None,
            )),
            Some(BestLevel::new(
                Fixed::from_str("101").unwrap(),
                Fixed::from_str("2").unwrap(),
                None,
            )),
            SourceKind::Bbo,
        )
    }

    #[test]
    fn groups_markets_by_symbol_when_available() {
        let lighter = tick(Venue::Lighter, "0", Some("ETH"));
        assert_eq!(market_key(&lighter), "ETH");

        let hyperliquid = tick(Venue::Hyperliquid, "ETH", Some("ETH"));
        assert_eq!(market_key(&hyperliquid), "ETH");
    }

    #[test]
    fn snapshot_tracks_venues_and_markets() {
        let mut store = BboStore::default();
        store.update(tick(Venue::Hyperliquid, "BTC", Some("BTC")));
        store.update(tick(Venue::Lighter, "0", Some("ETH")));

        let snapshot = store.snapshot();
        assert_eq!(snapshot.venues.len(), 2);
        assert_eq!(snapshot.markets, vec!["BTC".to_string(), "ETH".to_string()]);
        assert!(snapshot.find(Venue::Hyperliquid, "BTC").is_some());
    }
}
