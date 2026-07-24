use crate::domain::BboTick;

pub fn normalize(mut tick: BboTick, stale_after_ms: i64) -> BboTick {
    mark_invalid_levels(&mut tick);

    if let (Some(bid), Some(ask)) = (&tick.bid, &tick.ask) {
        tick.spread = ask.price.checked_sub(bid.price).ok();
        tick.mid = bid.price.midpoint(ask.price).ok();

        if let Some(spread) = tick.spread {
            if spread.value() < 0 {
                tick.quality.inconsistent = true;
                tick.quality.add_note("negative spread");
            }
        }
    }

    mark_stale(&mut tick, stale_after_ms);

    tick
}

fn mark_invalid_levels(tick: &mut BboTick) {
    let zero = crate::domain::Fixed::new(0, 0);
    let mut notes = Vec::new();

    if let Some(bid) = &tick.bid {
        if bid.price <= zero {
            notes.push("non-positive bid price");
        }
        if bid.size <= zero {
            notes.push("non-positive bid size");
        }
    }
    if let Some(ask) = &tick.ask {
        if ask.price <= zero {
            notes.push("non-positive ask price");
        }
        if ask.size <= zero {
            notes.push("non-positive ask size");
        }
    }

    for note in notes {
        tick.quality.inconsistent = true;
        tick.quality.add_note(note);
    }
}

fn mark_stale(tick: &mut BboTick, stale_after_ms: i64) {
    if stale_after_ms <= 0 {
        return;
    }

    let Some(exchange_ts_ms) = tick.exchange_ts_ms else {
        return;
    };

    let recv_ts_ms = tick.recv_ts_ns / 1_000_000;
    let age_ms = recv_ts_ms - i128::from(exchange_ts_ms);
    if age_ms > i128::from(stale_after_ms) {
        tick.quality.stale = true;
        tick.quality.add_note(format!("stale by {age_ms}ms"));
    } else if age_ms < -i128::from(stale_after_ms) {
        tick.quality.inconsistent = true;
        tick.quality
            .add_note(format!("exchange timestamp is {}ms in the future", -age_ms));
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use crate::{
        domain::{BboTick, BestLevel, Fixed, InstrumentCatalog, ProductType, SourceKind},
        pipeline::normalizer::normalize,
    };

    fn catalog() -> InstrumentCatalog {
        InstrumentCatalog::new(
            "hyperliquid",
            "BTC",
            "BTC",
            Some("BTC".to_string()),
            ProductType::Perp,
            "BTC",
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

    #[test]
    fn calculates_spread_and_mid() {
        let tick = BboTick::new(
            catalog().instrument_ref(),
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

        let tick = normalize(tick, 5_000);
        assert_eq!(tick.spread.unwrap().to_string(), "0.04");
        assert_eq!(tick.mid.unwrap().to_string(), "100.12");
        assert!(!tick.quality.inconsistent);
    }

    #[test]
    fn marks_crossed_bbo_inconsistent() {
        let tick = BboTick::new(
            catalog().instrument_ref(),
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

        let tick = normalize(tick, 5_000);
        assert!(tick.quality.inconsistent);
        assert_eq!(tick.quality.note.as_deref(), Some("negative spread"));
    }

    #[test]
    fn appends_negative_spread_note_to_existing_note() {
        let mut tick = BboTick::new(
            catalog().instrument_ref(),
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
        tick.quality.note = Some("checksum mismatch".to_string());

        let tick = normalize(tick, 5_000);
        assert_eq!(
            tick.quality.note.as_deref(),
            Some("checksum mismatch; negative spread")
        );
    }

    #[test]
    fn marks_ticks_stale_when_exchange_time_lags_receive_time() {
        let tick = BboTick::new(
            catalog().instrument_ref(),
            1_800_000_010_000_000_000,
            Some(1_800_000_000_000),
            None,
            None,
            None,
            SourceKind::Bbo,
        );

        let tick = normalize(tick, 5_000);
        assert!(tick.quality.stale);
        assert_eq!(tick.quality.note.as_deref(), Some("stale by 10000ms"));
    }

    #[test]
    fn marks_non_positive_prices_and_sizes_inconsistent() {
        let tick = BboTick::new(
            catalog().instrument_ref(),
            1_800_000_000_000_000_000,
            None,
            None,
            Some(BestLevel::new(
                Fixed::from_str("0").unwrap(),
                Fixed::from_str("-1").unwrap(),
                None,
            )),
            Some(BestLevel::new(
                Fixed::from_str("100").unwrap(),
                Fixed::from_str("0").unwrap(),
                None,
            )),
            SourceKind::Bbo,
        );

        let tick = normalize(tick, 5_000);

        assert!(tick.quality.inconsistent);
        assert_eq!(
            tick.quality.note.as_deref(),
            Some("non-positive bid price; non-positive bid size; non-positive ask size")
        );
    }

    #[test]
    fn marks_exchange_timestamps_far_in_the_future_inconsistent() {
        let tick = BboTick::new(
            catalog().instrument_ref(),
            1_800_000_000_000_000_000,
            Some(1_800_000_010_000),
            None,
            None,
            None,
            SourceKind::Bbo,
        );

        let tick = normalize(tick, 5_000);

        assert!(tick.quality.inconsistent);
        assert_eq!(
            tick.quality.note.as_deref(),
            Some("exchange timestamp is 10000ms in the future")
        );
    }
}
