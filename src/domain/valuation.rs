use std::collections::{BTreeSet, HashMap};

use serde::{Deserialize, Serialize};

use super::Fixed;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuoteRate {
    pub from: String,
    pub to: String,
    pub rate: Fixed,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuoteRateBook {
    rates: HashMap<(String, String), Fixed>,
}

impl QuoteRateBook {
    pub fn new(rates: impl IntoIterator<Item = QuoteRate>) -> Self {
        Self {
            rates: rates
                .into_iter()
                .map(|rate| ((rate.from, rate.to), rate.rate))
                .collect(),
        }
    }

    pub fn rate(&self, from: &str, to: &str) -> Option<Fixed> {
        if from == to {
            return Some(Fixed::new(1, 0));
        }

        self.rates.get(&(from.to_string(), to.to_string())).copied()
    }

    pub fn common_quote(&self, first: &str, second: &str) -> Option<String> {
        if first == second {
            return Some(first.to_string());
        }

        if self.rate(first, second).is_some() {
            return Some(second.to_string());
        }

        if self.rate(second, first).is_some() {
            return Some(first.to_string());
        }

        // Use the configured rate graph as the source of truth for canonical quote choices.
        let first_targets = self.convertible_targets(first);
        let second_targets = self.convertible_targets(second);
        first_targets
            .intersection(&second_targets)
            .next()
            .map(ToString::to_string)
    }

    fn convertible_targets(&self, asset: &str) -> BTreeSet<String> {
        let mut targets = BTreeSet::from([asset.to_string()]);
        targets.extend(
            self.rates
                .keys()
                .filter_map(|(from, to)| (from == asset).then_some(to.clone())),
        );
        targets
    }
}

#[cfg(test)]
mod tests {
    use super::{QuoteRate, QuoteRateBook};

    fn rate(from: &str, to: &str, rate: &str) -> QuoteRate {
        QuoteRate {
            from: from.to_string(),
            to: to.to_string(),
            rate: rate.parse().unwrap(),
        }
    }

    #[test]
    fn finds_common_quote_from_configured_rate_graph() {
        let book = QuoteRateBook::new([
            rate("USDC", "USD", "1"),
            rate("USDT", "USD", "1"),
            rate("DAI", "USDC", "1"),
        ]);

        assert_eq!(book.common_quote("USDC", "USDT").as_deref(), Some("USD"));
        assert_eq!(book.common_quote("DAI", "USDC").as_deref(), Some("USDC"));
    }

    #[test]
    fn direct_rate_takes_priority_over_canonical_target() {
        let book = QuoteRateBook::new([
            rate("USDC", "USD", "1"),
            rate("USDT", "USD", "1"),
            rate("USDC", "USDT", "0.9998"),
        ]);

        assert_eq!(book.common_quote("USDC", "USDT").as_deref(), Some("USDT"));
    }
}
