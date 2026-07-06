use std::collections::HashMap;

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
}
