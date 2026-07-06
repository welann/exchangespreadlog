use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::Fixed;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct InstrumentRef {
    pub catalog_id: String,
    pub venue_instance_id: String,
    pub instrument_id: String,
}

impl InstrumentRef {
    pub fn new(
        catalog_id: impl Into<String>,
        venue_instance_id: impl Into<String>,
        instrument_id: impl Into<String>,
    ) -> Self {
        Self {
            catalog_id: catalog_id.into(),
            venue_instance_id: venue_instance_id.into(),
            instrument_id: instrument_id.into(),
        }
    }

    pub fn unchecked(
        venue_instance_id: impl Into<String>,
        instrument_id: impl Into<String>,
    ) -> Self {
        let venue_instance_id = venue_instance_id.into();
        let instrument_id = instrument_id.into();
        let catalog_id = stable_catalog_id(&venue_instance_id, &instrument_id, "");
        Self {
            catalog_id,
            venue_instance_id,
            instrument_id,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProductType {
    Spot,
    Perp,
    Future,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PriceConvention {
    QuotePerBase,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SizeUnit {
    BaseAsset,
    Contracts,
}

/// Low-frequency market metadata. BBO ticks store only `InstrumentRef`; UI and
/// storage joins use this catalog entry to recover base/quote assets and rules.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InstrumentCatalog {
    pub catalog_id: String,
    pub venue_instance_id: String,
    pub instrument_id: String,
    pub raw_symbol: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub feed_symbol: Option<String>,
    pub product_type: ProductType,
    pub base_asset: String,
    pub quote_asset: String,
    pub settle_asset: String,
    pub margin_asset: String,
    pub price_convention: PriceConvention,
    pub size_unit: SizeUnit,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub price_tick: Option<Fixed>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_tick: Option<Fixed>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_size: Option<Fixed>,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_raw_json: Option<Value>,
}

impl InstrumentCatalog {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        venue_instance_id: impl Into<String>,
        instrument_id: impl Into<String>,
        raw_symbol: impl Into<String>,
        feed_symbol: Option<String>,
        product_type: ProductType,
        base_asset: impl Into<String>,
        quote_asset: impl Into<String>,
        settle_asset: impl Into<String>,
        margin_asset: impl Into<String>,
        price_tick: Option<Fixed>,
        size_tick: Option<Fixed>,
        min_size: Option<Fixed>,
        status: impl Into<String>,
        source_raw_json: Option<Value>,
    ) -> Self {
        Self::new_with_units(
            venue_instance_id,
            instrument_id,
            raw_symbol,
            feed_symbol,
            product_type,
            base_asset,
            quote_asset,
            settle_asset,
            margin_asset,
            PriceConvention::QuotePerBase,
            SizeUnit::BaseAsset,
            price_tick,
            size_tick,
            min_size,
            status,
            source_raw_json,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new_with_units(
        venue_instance_id: impl Into<String>,
        instrument_id: impl Into<String>,
        raw_symbol: impl Into<String>,
        feed_symbol: Option<String>,
        product_type: ProductType,
        base_asset: impl Into<String>,
        quote_asset: impl Into<String>,
        settle_asset: impl Into<String>,
        margin_asset: impl Into<String>,
        price_convention: PriceConvention,
        size_unit: SizeUnit,
        price_tick: Option<Fixed>,
        size_tick: Option<Fixed>,
        min_size: Option<Fixed>,
        status: impl Into<String>,
        source_raw_json: Option<Value>,
    ) -> Self {
        let venue_instance_id = venue_instance_id.into();
        let instrument_id = instrument_id.into();
        let raw_symbol = raw_symbol.into();
        let base_asset = base_asset.into();
        let quote_asset = quote_asset.into();
        let settle_asset = settle_asset.into();
        let margin_asset = margin_asset.into();
        let status = status.into();
        let price_tick_seed = price_tick
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_default();
        let size_tick_seed = size_tick
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_default();
        let min_size_seed = min_size
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_default();
        let version_seed = [
            raw_symbol.as_str(),
            feed_symbol.as_deref().unwrap_or(""),
            product_type.as_str(),
            base_asset.as_str(),
            quote_asset.as_str(),
            settle_asset.as_str(),
            margin_asset.as_str(),
            price_convention.as_str(),
            size_unit.as_str(),
            price_tick_seed.as_str(),
            size_tick_seed.as_str(),
            min_size_seed.as_str(),
            status.as_str(),
        ]
        .join("|");

        Self {
            catalog_id: stable_catalog_id(&venue_instance_id, &instrument_id, &version_seed),
            venue_instance_id,
            instrument_id,
            raw_symbol,
            feed_symbol,
            product_type,
            base_asset,
            quote_asset,
            settle_asset,
            margin_asset,
            price_convention,
            size_unit,
            price_tick,
            size_tick,
            min_size,
            status,
            source_raw_json,
        }
    }

    pub fn instrument_ref(&self) -> InstrumentRef {
        InstrumentRef::new(
            self.catalog_id.clone(),
            self.venue_instance_id.clone(),
            self.instrument_id.clone(),
        )
    }

    pub fn feed_key(&self) -> &str {
        self.feed_symbol.as_deref().unwrap_or(&self.instrument_id)
    }

    pub fn display_symbol(&self) -> &str {
        if self.raw_symbol.is_empty() {
            &self.instrument_id
        } else {
            &self.raw_symbol
        }
    }
}

impl ProductType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Spot => "spot",
            Self::Perp => "perp",
            Self::Future => "future",
        }
    }
}

impl PriceConvention {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::QuotePerBase => "quote_per_base",
        }
    }
}

impl SizeUnit {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::BaseAsset => "base_asset",
            Self::Contracts => "contracts",
        }
    }
}

fn stable_catalog_id(venue_instance_id: &str, instrument_id: &str, version_seed: &str) -> String {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in format!("{venue_instance_id}|{instrument_id}|{version_seed}").bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{venue_instance_id}:{instrument_id}:{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::{InstrumentCatalog, PriceConvention, ProductType, SizeUnit};

    #[test]
    fn catalog_id_changes_when_market_rules_change() {
        let first = InstrumentCatalog::new(
            "lighter",
            "0",
            "BTC",
            Some("0".to_string()),
            ProductType::Perp,
            "BTC",
            "USDC",
            "USDC",
            "USDC",
            "0.1".parse().ok(),
            None,
            None,
            "active",
            None,
        );
        let second = InstrumentCatalog::new(
            "lighter",
            "0",
            "BTC",
            Some("0".to_string()),
            ProductType::Perp,
            "BTC",
            "USDC",
            "USDC",
            "USDC",
            "0.01".parse().ok(),
            None,
            None,
            "active",
            None,
        );

        assert_ne!(first.catalog_id, second.catalog_id);
        assert_eq!(first.instrument_ref().instrument_id, "0");
    }

    #[test]
    fn catalog_id_changes_when_quote_or_size_semantics_change() {
        let first = InstrumentCatalog::new_with_units(
            "lighter",
            "0",
            "BTC",
            Some("0".to_string()),
            ProductType::Perp,
            "BTC",
            "USDC",
            "USDC",
            "USDC",
            PriceConvention::QuotePerBase,
            SizeUnit::BaseAsset,
            None,
            None,
            None,
            "active",
            None,
        );
        let second = InstrumentCatalog::new_with_units(
            "lighter",
            "0",
            "BTC",
            Some("0".to_string()),
            ProductType::Perp,
            "BTC",
            "USDC",
            "USDC",
            "USDC",
            PriceConvention::QuotePerBase,
            SizeUnit::Contracts,
            None,
            None,
            None,
            "active",
            None,
        );

        assert_ne!(first.catalog_id, second.catalog_id);
    }
}
