use crate::domain::BboTick;

pub fn normalize(mut tick: BboTick) -> BboTick {
    if let (Some(bid), Some(ask)) = (&tick.bid, &tick.ask) {
        tick.spread = ask.price.checked_sub(bid.price).ok();
        tick.mid = bid.price.midpoint(ask.price).ok();

        if let Some(spread) = tick.spread {
            if spread.value() < 0 {
                tick.quality.inconsistent = true;
                tick.quality.note = Some("negative spread".to_string());
            }
        }
    }

    tick
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use crate::{
        domain::{BboTick, BestLevel, Fixed, MarketRef, SourceKind, Venue},
        pipeline::normalizer::normalize,
    };

    #[test]
    fn calculates_spread_and_mid() {
        let tick = BboTick::new(
            Venue::Hyperliquid,
            MarketRef::new("BTC", Some("BTC".to_string())),
            123,
            Some(456),
            None,
            Some(BestLevel::new(
                Fixed::from_str("100.10").unwrap(),
                Fixed::from_str("1.5").unwrap(),
                None,
            )),
            Some(BestLevel::new(
                Fixed::from_str("100.14").unwrap(),
                Fixed::from_str("0.8").unwrap(),
                None,
            )),
            SourceKind::Bbo,
        );

        let tick = normalize(tick);
        assert_eq!(tick.spread.unwrap().to_string(), "0.04");
        assert_eq!(tick.mid.unwrap().to_string(), "100.12");
        assert!(!tick.quality.inconsistent);
    }

    #[test]
    fn marks_crossed_bbo_inconsistent() {
        let tick = BboTick::new(
            Venue::Hyperliquid,
            MarketRef::new("BTC", Some("BTC".to_string())),
            123,
            Some(456),
            None,
            Some(BestLevel::new(
                Fixed::from_str("101").unwrap(),
                Fixed::from_str("1").unwrap(),
                None,
            )),
            Some(BestLevel::new(
                Fixed::from_str("100").unwrap(),
                Fixed::from_str("1").unwrap(),
                None,
            )),
            SourceKind::Bbo,
        );

        let tick = normalize(tick);
        assert!(tick.quality.inconsistent);
        assert_eq!(tick.quality.note.as_deref(), Some("negative spread"));
    }
}
