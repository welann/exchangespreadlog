use std::{
    collections::{BTreeSet, HashMap},
    sync::{Arc, RwLock},
};

use crate::domain::{BboTick, InstrumentCatalog, QuoteRateBook};

pub type SharedBboState = Arc<RwLock<BboStore>>;

pub fn new_shared_state(rates: QuoteRateBook) -> SharedBboState {
    Arc::new(RwLock::new(BboStore::new(rates)))
}

#[derive(Debug, Default)]
pub struct BboStore {
    catalogs: HashMap<String, InstrumentCatalog>,
    ticks: HashMap<String, BboTick>,
    rates: QuoteRateBook,
}

#[derive(Debug, Clone, Default)]
pub struct BboSnapshot {
    pub ticks: Vec<BboTick>,
    pub catalogs: HashMap<String, InstrumentCatalog>,
    pub markets: Vec<String>,
    pub rates: QuoteRateBook,
}

pub struct BboRow<'a> {
    pub tick: &'a BboTick,
    pub catalog: Option<&'a InstrumentCatalog>,
}

impl BboStore {
    pub fn new(rates: QuoteRateBook) -> Self {
        Self {
            rates,
            ..Self::default()
        }
    }

    pub fn update_catalog(&mut self, catalog: InstrumentCatalog) {
        self.catalogs.insert(catalog.catalog_id.clone(), catalog);
    }

    pub fn update_tick(&mut self, tick: BboTick) {
        self.ticks.insert(tick.instrument.catalog_id.clone(), tick);
    }

    pub fn reset_venue(&mut self, venue_instance_id: &str) {
        self.catalogs
            .retain(|_, catalog| catalog.venue_instance_id != venue_instance_id);
        self.ticks
            .retain(|_, tick| tick.instrument.venue_instance_id != venue_instance_id);
    }

    pub fn snapshot(&self) -> BboSnapshot {
        let mut ticks = self.ticks.values().cloned().collect::<Vec<_>>();
        ticks.sort_by(|lhs, rhs| {
            market_key_for(&self.catalogs, lhs)
                .cmp(&market_key_for(&self.catalogs, rhs))
                .then(
                    instrument_label_for(&self.catalogs, lhs)
                        .cmp(&instrument_label_for(&self.catalogs, rhs)),
                )
        });

        let markets = ticks
            .iter()
            .map(|tick| market_key_for(&self.catalogs, tick))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();

        BboSnapshot {
            ticks,
            catalogs: self.catalogs.clone(),
            markets,
            rates: self.rates.clone(),
        }
    }
}

impl BboSnapshot {
    pub fn catalog_for(&self, tick: &BboTick) -> Option<&InstrumentCatalog> {
        self.catalogs.get(&tick.instrument.catalog_id)
    }

    pub fn rows_for_market(&self, market: &str) -> Vec<BboRow<'_>> {
        self.ticks
            .iter()
            .filter(|tick| self.market_key(tick) == market)
            .map(|tick| BboRow {
                tick,
                catalog: self.catalog_for(tick),
            })
            .collect()
    }

    pub fn row_for_market(&self, market: &str, index: usize) -> Option<BboRow<'_>> {
        self.rows_for_market(market).into_iter().nth(index)
    }

    pub fn market_key(&self, tick: &BboTick) -> String {
        market_key_for(&self.catalogs, tick)
    }

    pub fn instrument_label(&self, tick: &BboTick) -> String {
        instrument_label_for(&self.catalogs, tick)
    }
}

fn market_key_for(catalogs: &HashMap<String, InstrumentCatalog>, tick: &BboTick) -> String {
    catalogs
        .get(&tick.instrument.catalog_id)
        .map(|catalog| catalog.base_asset.clone())
        .unwrap_or_else(|| tick.instrument.instrument_id.clone())
}

fn instrument_label_for(catalogs: &HashMap<String, InstrumentCatalog>, tick: &BboTick) -> String {
    catalogs
        .get(&tick.instrument.catalog_id)
        .map(|catalog| {
            format!(
                "{}/{} {}",
                catalog.venue_instance_id,
                catalog.display_symbol(),
                catalog.quote_asset
            )
        })
        .unwrap_or_else(|| {
            format!(
                "{}/{}",
                tick.instrument.venue_instance_id, tick.instrument.instrument_id
            )
        })
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use crate::{
        domain::{
            BboTick, BestLevel, Fixed, InstrumentCatalog, ProductType, QuoteRateBook, SourceKind,
        },
        state::BboStore,
    };

    fn catalog(
        venue: &str,
        id: &str,
        raw_symbol: &str,
        base: &str,
        quote: &str,
    ) -> InstrumentCatalog {
        InstrumentCatalog::new(
            venue,
            id,
            raw_symbol,
            Some(id.to_string()),
            ProductType::Perp,
            base,
            quote,
            quote,
            quote,
            None,
            None,
            None,
            "active",
            None,
        )
    }

    fn tick(catalog: &InstrumentCatalog) -> BboTick {
        BboTick::new(
            catalog.instrument_ref(),
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
    fn keeps_multiple_instruments_for_same_base_asset() {
        let mut store = BboStore::new(QuoteRateBook::default());
        let usdc = catalog("dex", "BTC-USDC", "BTC-USDC", "BTC", "USDC");
        let usdt = catalog("dex", "BTC-USDT", "BTC-USDT", "BTC", "USDT");
        store.update_catalog(usdc.clone());
        store.update_catalog(usdt.clone());
        store.update_tick(tick(&usdc));
        store.update_tick(tick(&usdt));

        let snapshot = store.snapshot();
        assert_eq!(snapshot.markets, vec!["BTC".to_string()]);
        assert_eq!(snapshot.rows_for_market("BTC").len(), 2);
    }

    #[test]
    fn reset_venue_removes_only_that_venues_catalogs_and_ticks() {
        let mut store = BboStore::new(QuoteRateBook::default());
        let lighter = catalog("lighter", "1", "BTC", "BTC", "USDC");
        let hyperliquid = catalog("hyperliquid", "BTC", "BTC", "BTC", "USDC");
        for instrument in [&lighter, &hyperliquid] {
            store.update_catalog(instrument.clone());
            store.update_tick(tick(instrument));
        }

        store.reset_venue("lighter");

        let snapshot = store.snapshot();
        assert_eq!(snapshot.ticks.len(), 1);
        assert_eq!(snapshot.catalogs.len(), 1);
        assert_eq!(
            snapshot.ticks[0].instrument.venue_instance_id,
            "hyperliquid"
        );
    }
}
