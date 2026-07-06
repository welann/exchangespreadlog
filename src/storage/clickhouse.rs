use anyhow::{Context, anyhow, bail};
use async_trait::async_trait;
use reqwest::StatusCode;
use serde::Serialize;
use tokio::sync::Mutex;
use tracing::warn;

use crate::{
    config::ClickHouseConfig,
    domain::{BboTick, BestLevel, Fixed, InstrumentCatalog, SourceKind},
    storage::BboSink,
};

pub struct ClickHouseSink {
    client: reqwest::Client,
    url: String,
    database: String,
    table: String,
    catalog_table: String,
    username: String,
    password: Option<String>,
    batch_size: usize,
    buffer: Mutex<Vec<BboClickHouseRow>>,
}

impl ClickHouseSink {
    pub async fn new(config: &ClickHouseConfig) -> anyhow::Result<Self> {
        let url = config.url.trim().trim_end_matches('/').to_string();
        if url.is_empty() {
            bail!("ClickHouse url cannot be empty");
        }

        let database = validate_identifier("ClickHouse database", &config.database)?;
        let table = validate_identifier("ClickHouse table", &config.table)?;
        let catalog_table = validate_identifier("ClickHouse catalog table", &config.catalog_table)?;
        let password = resolve_password(config)?;
        let batch_size = config.batch_size.max(1);
        if config.batch_size == 0 {
            warn!("ClickHouse batch_size is 0; using 1 because every tick must be flushed");
        }

        let sink = Self {
            client: reqwest::Client::new(),
            url,
            database,
            table,
            catalog_table,
            username: config.username.clone(),
            password,
            batch_size,
            buffer: Mutex::new(Vec::with_capacity(batch_size)),
        };

        if config.create_table {
            sink.ensure_table()
                .await
                .context("initialize ClickHouse table")?;
        }

        Ok(sink)
    }

    async fn ensure_table(&self) -> anyhow::Result<()> {
        let table = quote_identifier(&self.table);
        let catalog_table = quote_identifier(&self.catalog_table);
        let catalog_sql = format!(
            r#"CREATE TABLE IF NOT EXISTS {catalog_table}
(
    catalog_id String,
    venue_instance_id LowCardinality(String),
    instrument_id String,
    raw_symbol String,
    feed_symbol Nullable(String),
    product_type LowCardinality(String),
    base_asset LowCardinality(String),
    quote_asset LowCardinality(String),
    settle_asset LowCardinality(String),
    margin_asset LowCardinality(String),
    price_convention LowCardinality(String),
    size_unit LowCardinality(String),
    price_tick Nullable(String),
    size_tick Nullable(String),
    min_size Nullable(String),
    status LowCardinality(String),
    source_raw_json Nullable(String),
    inserted_time DateTime64(9, 'UTC') DEFAULT now64(9)
)
ENGINE = ReplacingMergeTree(inserted_time)
ORDER BY (venue_instance_id, instrument_id, catalog_id)"#
        );
        self.execute_sql(catalog_sql).await?;

        let tick_sql = format!(
            r#"CREATE TABLE IF NOT EXISTS {table}
(
    catalog_id String,
    venue_instance_id LowCardinality(String),
    instrument_id String,
    recv_ts_ns Int64,
    recv_time DateTime64(9, 'UTC') MATERIALIZED fromUnixTimestamp64Nano(recv_ts_ns),
    exchange_ts_ms Nullable(Int64),
    sequence Nullable(String),
    source LowCardinality(String),
    bid_price Nullable(Float64),
    bid_price_text Nullable(String),
    bid_size Nullable(Float64),
    bid_size_text Nullable(String),
    bid_order_count Nullable(UInt32),
    ask_price Nullable(Float64),
    ask_price_text Nullable(String),
    ask_size Nullable(Float64),
    ask_size_text Nullable(String),
    ask_order_count Nullable(UInt32),
    spread Nullable(Float64),
    spread_text Nullable(String),
    mid Nullable(Float64),
    mid_text Nullable(String),
    quality_gap Bool,
    quality_stale Bool,
    quality_inconsistent Bool,
    quality_note Nullable(String)
)
ENGINE = MergeTree
PARTITION BY toDate(recv_time)
ORDER BY (venue_instance_id, instrument_id, recv_time)"#
        );
        self.execute_sql(tick_sql).await
    }

    async fn insert_tick_rows(&self, rows: &[BboClickHouseRow]) -> anyhow::Result<()> {
        if rows.is_empty() {
            return Ok(());
        }

        let mut sql = format!(
            "INSERT INTO {} FORMAT JSONEachRow\n",
            quote_identifier(&self.table)
        );
        for row in rows {
            sql.push_str(&serde_json::to_string(row).context("serialize ClickHouse row")?);
            sql.push('\n');
        }

        self.execute_sql(sql).await
    }

    async fn insert_catalog_row(&self, row: &CatalogClickHouseRow) -> anyhow::Result<()> {
        let sql = format!(
            "INSERT INTO {} FORMAT JSONEachRow\n{}\n",
            quote_identifier(&self.catalog_table),
            serde_json::to_string(row).context("serialize ClickHouse catalog row")?
        );
        self.execute_sql(sql).await
    }

    async fn execute_sql(&self, sql: String) -> anyhow::Result<()> {
        let mut request = self
            .client
            .post(&self.url)
            .query(&[("database", self.database.as_str())])
            .header("Content-Type", "text/plain; charset=utf-8")
            .body(sql);

        if !self.username.is_empty() {
            request = request.basic_auth(&self.username, self.password.as_deref());
        }

        let response = request
            .send()
            .await
            .with_context(|| format!("send ClickHouse request to {}", self.url))?;
        let status = response.status();
        let body = response
            .text()
            .await
            .unwrap_or_else(|err| format!("<failed to read response body: {err}>"));

        if status.is_success() {
            Ok(())
        } else {
            Err(clickhouse_status_error(status, &body))
        }
    }
}

#[async_trait]
impl BboSink for ClickHouseSink {
    async fn write_catalog(&self, catalog: &InstrumentCatalog) -> anyhow::Result<()> {
        self.insert_catalog_row(&CatalogClickHouseRow::from_catalog(catalog))
            .await
    }

    async fn write_tick(&self, tick: &BboTick) -> anyhow::Result<()> {
        let row = BboClickHouseRow::try_from_tick(tick)?;
        let mut ready = None;

        {
            let mut buffer = self.buffer.lock().await;
            buffer.push(row);
            if buffer.len() >= self.batch_size {
                ready = Some(std::mem::take(&mut *buffer));
            }
        }

        if let Some(mut rows) = ready {
            if let Err(err) = self.insert_tick_rows(&rows).await {
                let mut buffer = self.buffer.lock().await;
                rows.append(&mut *buffer);
                *buffer = rows;
                return Err(err);
            }
        }

        Ok(())
    }

    async fn flush(&self) -> anyhow::Result<()> {
        let mut rows = {
            let mut buffer = self.buffer.lock().await;
            std::mem::take(&mut *buffer)
        };

        if rows.is_empty() {
            return Ok(());
        }

        if let Err(err) = self.insert_tick_rows(&rows).await {
            let mut buffer = self.buffer.lock().await;
            rows.append(&mut *buffer);
            *buffer = rows;
            return Err(err);
        }

        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct CatalogClickHouseRow {
    catalog_id: String,
    venue_instance_id: String,
    instrument_id: String,
    raw_symbol: String,
    feed_symbol: Option<String>,
    product_type: String,
    base_asset: String,
    quote_asset: String,
    settle_asset: String,
    margin_asset: String,
    price_convention: String,
    size_unit: String,
    price_tick: Option<String>,
    size_tick: Option<String>,
    min_size: Option<String>,
    status: String,
    source_raw_json: Option<String>,
}

impl CatalogClickHouseRow {
    fn from_catalog(catalog: &InstrumentCatalog) -> Self {
        Self {
            catalog_id: catalog.catalog_id.clone(),
            venue_instance_id: catalog.venue_instance_id.clone(),
            instrument_id: catalog.instrument_id.clone(),
            raw_symbol: catalog.raw_symbol.clone(),
            feed_symbol: catalog.feed_symbol.clone(),
            product_type: catalog.product_type.as_str().to_string(),
            base_asset: catalog.base_asset.clone(),
            quote_asset: catalog.quote_asset.clone(),
            settle_asset: catalog.settle_asset.clone(),
            margin_asset: catalog.margin_asset.clone(),
            price_convention: catalog.price_convention.as_str().to_string(),
            size_unit: catalog.size_unit.as_str().to_string(),
            price_tick: catalog.price_tick.map(|value| value.to_string()),
            size_tick: catalog.size_tick.map(|value| value.to_string()),
            min_size: catalog.min_size.map(|value| value.to_string()),
            status: catalog.status.clone(),
            source_raw_json: catalog
                .source_raw_json
                .as_ref()
                .map(|value| value.to_string()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct BboClickHouseRow {
    catalog_id: String,
    venue_instance_id: String,
    instrument_id: String,
    recv_ts_ns: i64,
    exchange_ts_ms: Option<i64>,
    sequence: Option<String>,
    source: String,
    bid_price: Option<f64>,
    bid_price_text: Option<String>,
    bid_size: Option<f64>,
    bid_size_text: Option<String>,
    bid_order_count: Option<u32>,
    ask_price: Option<f64>,
    ask_price_text: Option<String>,
    ask_size: Option<f64>,
    ask_size_text: Option<String>,
    ask_order_count: Option<u32>,
    spread: Option<f64>,
    spread_text: Option<String>,
    mid: Option<f64>,
    mid_text: Option<String>,
    quality_gap: bool,
    quality_stale: bool,
    quality_inconsistent: bool,
    quality_note: Option<String>,
}

impl BboClickHouseRow {
    fn try_from_tick(tick: &BboTick) -> anyhow::Result<Self> {
        let (bid_price, bid_price_text, bid_size, bid_size_text, bid_order_count) =
            best_level_fields(tick.bid.as_ref());
        let (ask_price, ask_price_text, ask_size, ask_size_text, ask_order_count) =
            best_level_fields(tick.ask.as_ref());
        let (spread, spread_text) = fixed_fields(tick.spread);
        let (mid, mid_text) = fixed_fields(tick.mid);

        Ok(Self {
            recv_ts_ns: i64::try_from(tick.recv_ts_ns).with_context(|| {
                format!("recv_ts_ns {} does not fit into Int64", tick.recv_ts_ns)
            })?,
            exchange_ts_ms: tick.exchange_ts_ms,
            catalog_id: tick.instrument.catalog_id.clone(),
            venue_instance_id: tick.instrument.venue_instance_id.clone(),
            instrument_id: tick.instrument.instrument_id.clone(),
            sequence: tick.sequence.map(|value| value.to_string()),
            source: source_kind_as_str(tick.source).to_string(),
            bid_price,
            bid_price_text,
            bid_size,
            bid_size_text,
            bid_order_count,
            ask_price,
            ask_price_text,
            ask_size,
            ask_size_text,
            ask_order_count,
            spread,
            spread_text,
            mid,
            mid_text,
            quality_gap: tick.quality.gap,
            quality_stale: tick.quality.stale,
            quality_inconsistent: tick.quality.inconsistent,
            quality_note: tick.quality.note.clone(),
        })
    }
}

fn best_level_fields(
    level: Option<&BestLevel>,
) -> (
    Option<f64>,
    Option<String>,
    Option<f64>,
    Option<String>,
    Option<u32>,
) {
    let Some(level) = level else {
        return (None, None, None, None, None);
    };
    let (price, price_text) = fixed_fields(Some(level.price));
    let (size, size_text) = fixed_fields(Some(level.size));
    (price, price_text, size, size_text, level.order_count)
}

fn fixed_fields(value: Option<Fixed>) -> (Option<f64>, Option<String>) {
    match value {
        Some(value) => (Some(value.to_f64()), Some(value.to_string())),
        None => (None, None),
    }
}

fn source_kind_as_str(source: SourceKind) -> &'static str {
    match source {
        SourceKind::Bbo => "bbo",
        SourceKind::Ticker => "ticker",
        SourceKind::L2Book => "l2_book",
    }
}

fn resolve_password(config: &ClickHouseConfig) -> anyhow::Result<Option<String>> {
    if let Some(password) = config.password.as_deref() {
        if !password.is_empty() {
            return Ok(Some(password.to_string()));
        }
    }

    if let Some(env_name) = config.password_env.as_deref().map(str::trim) {
        if !env_name.is_empty() {
            let password = std::env::var(env_name)
                .with_context(|| format!("read ClickHouse password from ${env_name}"))?;
            return Ok(Some(password));
        }
    }

    Ok(None)
}

fn validate_identifier(label: &str, value: &str) -> anyhow::Result<String> {
    let value = value.trim();
    if value.is_empty() {
        bail!("{label} cannot be empty");
    }

    if !value
        .chars()
        .all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
    {
        bail!("{label} must contain only ASCII letters, numbers, or underscores");
    }

    Ok(value.to_string())
}

fn quote_identifier(value: &str) -> String {
    format!("`{value}`")
}

fn clickhouse_status_error(status: StatusCode, body: &str) -> anyhow::Error {
    let body = body.trim();
    let body = if body.chars().count() > 512 {
        format!("{}...", body.chars().take(512).collect::<String>())
    } else {
        body.to_string()
    };
    anyhow!("ClickHouse HTTP error {status}: {body}")
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use crate::domain::{BboTick, BestLevel, Fixed, InstrumentCatalog, ProductType, SourceKind};

    use super::{BboClickHouseRow, CatalogClickHouseRow, validate_identifier};

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
            "0.1".parse().ok(),
            None,
            None,
            "active",
            None,
        )
    }

    #[test]
    fn converts_bbo_tick_to_clickhouse_row() {
        let catalog = catalog();
        let mut tick = BboTick::new(
            catalog.instrument_ref(),
            1_800_000_000_000_000_000,
            Some(1_800_000_000_000),
            Some(42),
            Some(BestLevel::new(
                Fixed::from_str("100.10").unwrap(),
                Fixed::from_str("2.5").unwrap(),
                Some(3),
            )),
            Some(BestLevel::new(
                Fixed::from_str("100.20").unwrap(),
                Fixed::from_str("1.5").unwrap(),
                Some(4),
            )),
            SourceKind::Bbo,
        );
        tick.spread = Some(Fixed::from_str("0.10").unwrap());
        tick.mid = Some(Fixed::from_str("100.15").unwrap());

        let row = BboClickHouseRow::try_from_tick(&tick).unwrap();

        assert_eq!(row.recv_ts_ns, 1_800_000_000_000_000_000);
        assert_eq!(row.venue_instance_id, "hyperliquid");
        assert_eq!(row.instrument_id, "BTC");
        assert_eq!(row.sequence.as_deref(), Some("42"));
        assert_eq!(row.bid_price_text.as_deref(), Some("100.1"));
        assert_eq!(row.ask_size_text.as_deref(), Some("1.5"));
        assert_eq!(row.spread_text.as_deref(), Some("0.1"));
        assert_eq!(row.mid, Some(100.15));
    }

    #[test]
    fn converts_catalog_to_clickhouse_row() {
        let row = CatalogClickHouseRow::from_catalog(&catalog());

        assert_eq!(row.venue_instance_id, "hyperliquid");
        assert_eq!(row.instrument_id, "BTC");
        assert_eq!(row.product_type, "perp");
        assert_eq!(row.base_asset, "BTC");
        assert_eq!(row.quote_asset, "USDC");
        assert_eq!(row.price_convention, "quote_per_base");
        assert_eq!(row.size_unit, "base_asset");
        assert_eq!(row.price_tick.as_deref(), Some("0.1"));
    }

    #[test]
    fn rejects_unsafe_identifiers() {
        assert!(validate_identifier("table", "bbo_ticks").is_ok());
        assert!(validate_identifier("table", "bbo-ticks").is_err());
        assert!(validate_identifier("table", "bbo_ticks; DROP TABLE x").is_err());
    }
}
