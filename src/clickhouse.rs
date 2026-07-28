use std::{collections::BTreeMap, time::Duration};

use anyhow::{Context, bail};
use reqwest::Client;
use serde::{
    Deserialize, Deserializer, Serialize,
    de::{Error as SerdeError, Visitor},
};
use serde_json::{Value, json};
use tokio::{sync::watch, task, time};
use tracing::{info, warn};

use crate::{
    config::ClickHouseConfig,
    domain::{BboTick, InstrumentCatalog, MarketEvent, SourceKind},
    store::{PairConversion, SpreadPoint},
    wal::{DurableEventLog, WalRecord},
};

const MIGRATION: &str = include_str!("../migrations/001_init.sql");

#[derive(Clone)]
pub struct ClickHouse {
    client: Client,
    config: ClickHouseConfig,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryResponse {
    pub from_ms: i64,
    pub to_ms: i64,
    pub resolution_ms: i64,
    pub source_rows: usize,
    pub target_quote: String,
    pub points: Vec<SpreadPoint>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HistoryRow {
    instrument_key: String,
    #[serde(deserialize_with = "deserialize_i64")]
    bucket_ms: i64,
    #[serde(deserialize_with = "deserialize_i64")]
    recv_ts_ns: i64,
    bid_price: String,
    ask_price: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct VenueResetRow {
    venue: String,
    #[serde(deserialize_with = "deserialize_i64")]
    recv_ts_ns: i64,
}

fn deserialize_i64<'de, D>(deserializer: D) -> Result<i64, D::Error>
where
    D: Deserializer<'de>,
{
    struct I64Visitor;

    impl Visitor<'_> for I64Visitor {
        type Value = i64;

        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("an Int64 encoded as a JSON integer or string")
        }

        fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E> {
            Ok(value)
        }

        fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E>
        where
            E: SerdeError,
        {
            i64::try_from(value).map_err(E::custom)
        }

        fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
        where
            E: SerdeError,
        {
            value.parse().map_err(E::custom)
        }
    }

    deserializer.deserialize_any(I64Visitor)
}

impl ClickHouse {
    pub fn new(config: ClickHouseConfig) -> anyhow::Result<Self> {
        validate_identifier(&config.database)?;
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(15))
            .build()
            .context("build ClickHouse HTTP client")?;
        Ok(Self { client, config })
    }

    pub async fn ping(&self) -> anyhow::Result<String> {
        let rows = self
            .query_json("SELECT version() AS version FORMAT JSONEachRow")
            .await?;
        rows.first()
            .and_then(|row| row.get("version"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .context("ClickHouse ping response is missing version")
    }

    pub async fn ensure_schema(&self) -> anyhow::Result<()> {
        let database = quote_identifier(&self.config.database);
        for (index, statement) in MIGRATION.split("-- migrate:split").enumerate() {
            let sql = statement.trim();
            if sql.is_empty() {
                continue;
            }
            self.execute(sql.replace("{database}", &database))
                .await
                .with_context(|| format!("apply ClickHouse migration statement {}", index + 1))?;
        }
        info!("ClickHouse schema is ready");
        Ok(())
    }

    pub async fn run_projector(
        &self,
        wal: DurableEventLog,
        mut shutdown: watch::Receiver<bool>,
    ) -> anyhow::Result<()> {
        let mut schema_ready = false;
        info!(
            batch_size = self.config.projector_batch_size,
            linger_ms = self.config.projector_linger.as_millis(),
            "ClickHouse projector batching configured"
        );
        while !*shutdown.borrow() {
            if !schema_ready {
                match self
                    .ping()
                    .await
                    .context("connect to ClickHouse")
                    .map(|version| {
                        info!(%version, url = %self.config.url, "connected to ClickHouse");
                    }) {
                    Ok(()) => match self.ensure_schema().await {
                        Ok(()) => schema_ready = true,
                        Err(error) => {
                            warn!(%error, "ClickHouse schema setup failed; WAL retained");
                        }
                    },
                    Err(error) => {
                        warn!(%error, "ClickHouse unavailable; WAL retained");
                    }
                }
                if !schema_ready {
                    tokio::select! {
                        _ = time::sleep(Duration::from_secs(2)) => {}
                        changed = shutdown.changed() => {
                            if changed.is_err() || *shutdown.borrow() {
                                break;
                            }
                        }
                    }
                    continue;
                }
            }

            let Some(rows) = next_projector_batch(
                &wal,
                self.config.projector_batch_size,
                self.config.projector_linger,
                &mut shutdown,
            )
            .await?
            else {
                break;
            };
            match self.project_batch(&rows).await {
                Ok(()) => {
                    let last_seq = rows.last().expect("batch is non-empty").seq;
                    let wal_acknowledger = wal.clone();
                    task::spawn_blocking(move || wal_acknowledger.acknowledge_projector(last_seq))
                        .await
                        .context("join WAL projector checkpoint")??;
                }
                Err(error) => {
                    warn!(%error, rows = rows.len(), "ClickHouse projection failed; WAL retained");
                    tokio::select! {
                        _ = time::sleep(Duration::from_secs(2)) => {}
                        changed = shutdown.changed() => {
                            if changed.is_err() || *shutdown.borrow() {
                                break;
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }

    pub async fn history_spread(
        &self,
        leg_a: &str,
        leg_b: &str,
        from_ms: i64,
        to_ms: i64,
        venue_a: &str,
        venue_b: &str,
        conversion: PairConversion,
    ) -> anyhow::Result<HistoryResponse> {
        if from_ms >= to_ms {
            bail!("fromMs must be before toMs");
        }
        let max_range_ms = 31_i64 * 24 * 60 * 60 * 1_000;
        if to_ms - from_ms > max_range_ms {
            bail!("history range is capped at 31 days");
        }
        let (table, resolution_ms) = resolution(to_ms - from_ms);
        let (seed_before_ms, range_from_ms, range_before_ms) =
            complete_bucket_bounds(from_ms, to_ms, resolution_ms);
        let table = format!(
            "{}.{}",
            quote_identifier(&self.config.database),
            quote_identifier(table)
        );
        let legs = format!("{}, {}", quote_string(leg_a), quote_string(leg_b));
        let venues = format!("{}, {}", quote_string(venue_a), quote_string(venue_b));
        let range_sql = format!(
            r#"
SELECT
    instrument_key AS instrumentKey,
    toUnixTimestamp64Milli(bucket_time) AS bucketMs,
    max(recv_ts_ns) AS recvTsNs,
    toString(argMax(bid_price, recv_ts_ns)) AS bidPrice,
    toString(argMax(ask_price, recv_ts_ns)) AS askPrice
FROM {table}
WHERE instrument_key IN ({legs})
  AND bucket_time >= fromUnixTimestamp64Milli({range_from_ms})
  AND bucket_time < fromUnixTimestamp64Milli({range_before_ms})
GROUP BY instrument_key, bucket_time
ORDER BY bucket_time, instrument_key
FORMAT JSONEachRow
"#
        );
        let seed_sql = format!(
            r#"
SELECT
    instrument_key AS instrumentKey,
    toUnixTimestamp64Milli(argMax(bucket_time, recv_ts_ns)) AS bucketMs,
    max(recv_ts_ns) AS recvTsNs,
    toString(argMax(bid_price, recv_ts_ns)) AS bidPrice,
    toString(argMax(ask_price, recv_ts_ns)) AS askPrice
FROM {table}
WHERE instrument_key IN ({legs})
  AND bucket_time < fromUnixTimestamp64Milli({seed_before_ms})
GROUP BY instrument_key
FORMAT JSONEachRow
"#
        );
        let reset_table = format!(
            "{}.{}",
            quote_identifier(&self.config.database),
            quote_identifier("venue_state_events")
        );
        let resets_sql = format!(
            r#"
SELECT
    venue,
    recvTsNs
FROM
(
    SELECT
        venue,
        max(recv_ts_ns) AS recvTsNs
    FROM {reset_table}
    WHERE venue IN ({venues})
      AND recv_time < fromUnixTimestamp64Milli({from_ms})
    GROUP BY venue

    UNION ALL

    SELECT
        venue,
        recv_ts_ns AS recvTsNs
    FROM {reset_table}
    WHERE venue IN ({venues})
      AND recv_time >= fromUnixTimestamp64Milli({from_ms})
      AND recv_time < fromUnixTimestamp64Milli({to_ms})
)
ORDER BY recvTsNs
FORMAT JSONEachRow
"#
        );
        let (range, seed, resets) = tokio::try_join!(
            self.query_typed::<HistoryRow>(&range_sql),
            self.query_typed::<HistoryRow>(&seed_sql),
            self.query_typed::<VenueResetRow>(&resets_sql)
        )?;
        let source_rows = range.len();
        let points = align_history(
            leg_a,
            leg_b,
            venue_a,
            venue_b,
            HistoryWindow {
                from_ms,
                to_ms,
                resolution_ms,
            },
            seed,
            range,
            resets,
            conversion.a_rate,
            conversion.b_rate,
        )?;
        Ok(HistoryResponse {
            from_ms,
            to_ms,
            resolution_ms,
            source_rows,
            target_quote: conversion.target_quote,
            points,
        })
    }

    async fn project_batch(&self, records: &[WalRecord]) -> anyhow::Result<()> {
        let mut catalog_rows = Vec::new();
        let mut tick_rows = Vec::new();
        let mut venue_rows = Vec::new();
        for record in records {
            match &record.value.event {
                MarketEvent::Catalog { instrument } => {
                    catalog_rows.push(catalog_row(instrument, record.value.recorded_ts_ns)?);
                }
                MarketEvent::Tick { tick } => {
                    tick_rows.push(tick_row(&record.event_id, tick)?);
                }
                MarketEvent::VenueReset { venue_instance_id } => {
                    venue_rows.push(json!({
                        "event_id": record.event_id,
                        "venue": venue_instance_id,
                        "state": "disconnected",
                        "reason": "adapter_reset",
                        "recv_ts_ns": i64::try_from(record.value.recorded_ts_ns)
                            .context("venue reset timestamp exceeds Int64")?
                    }));
                }
            }
        }
        self.insert_json_rows("instrument_catalog", &catalog_rows)
            .await?;
        self.insert_json_rows("bbo_events", &tick_rows).await?;
        self.insert_json_rows("venue_state_events", &venue_rows)
            .await?;
        Ok(())
    }

    async fn insert_json_rows(&self, table: &str, rows: &[Value]) -> anyhow::Result<()> {
        if rows.is_empty() {
            return Ok(());
        }
        validate_identifier(table)?;
        let mut body = format!(
            "INSERT INTO {}.{} FORMAT JSONEachRow\n",
            quote_identifier(&self.config.database),
            quote_identifier(table)
        );
        for row in rows {
            body.push_str(&serde_json::to_string(row)?);
            body.push('\n');
        }
        self.execute(body).await
    }

    async fn query_typed<T>(&self, sql: &str) -> anyhow::Result<Vec<T>>
    where
        T: for<'de> Deserialize<'de>,
    {
        self.query_text(sql)
            .await?
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| serde_json::from_str(line).context("decode ClickHouse JSONEachRow"))
            .collect()
    }

    async fn query_json(&self, sql: &str) -> anyhow::Result<Vec<Value>> {
        self.query_typed(sql).await
    }

    async fn execute(&self, sql: String) -> anyhow::Result<()> {
        self.query_text(&sql).await.map(|_| ())
    }

    async fn query_text(&self, sql: &str) -> anyhow::Result<String> {
        let response = self
            .client
            .post(&self.config.url)
            .query(&[("database", self.config.database.as_str())])
            .basic_auth(&self.config.username, Some(&self.config.password))
            .header("Content-Type", "text/plain; charset=utf-8")
            .body(sql.to_string())
            .send()
            .await
            .with_context(|| format!("send ClickHouse request to {}", self.config.url))?;
        let status = response.status();
        let body = response.text().await.context("read ClickHouse response")?;
        if status.is_success() {
            Ok(body)
        } else {
            bail!(
                "ClickHouse HTTP {}: {}",
                status.as_u16(),
                body.chars().take(800).collect::<String>()
            )
        }
    }
}

async fn next_projector_batch(
    wal: &DurableEventLog,
    batch_size: usize,
    linger: Duration,
    shutdown: &mut watch::Receiver<bool>,
) -> anyhow::Result<Option<Vec<WalRecord>>> {
    loop {
        let rows = read_projector_rows(wal.clone(), batch_size).await?;
        if rows.is_empty() {
            tokio::select! {
                _ = wal.wait_for_append() => {}
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() {
                        return Ok(None);
                    }
                }
            }
            continue;
        }
        if rows.len() >= batch_size {
            return Ok(Some(rows));
        }

        let deadline = time::Instant::now() + linger;
        let mut rows = rows;
        loop {
            tokio::select! {
                _ = time::sleep_until(deadline) => return Ok(Some(rows)),
                _ = wal.wait_for_append() => {
                    rows = read_projector_rows(wal.clone(), batch_size).await?;
                    if rows.len() >= batch_size {
                        return Ok(Some(rows));
                    }
                }
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() {
                        return Ok(None);
                    }
                }
            }
        }
    }
}

async fn read_projector_rows(
    wal: DurableEventLog,
    batch_size: usize,
) -> anyhow::Result<Vec<WalRecord>> {
    task::spawn_blocking(move || wal.read_projector_batch(batch_size))
        .await
        .context("join WAL projector read")?
}

fn catalog_row(catalog: &InstrumentCatalog, recorded_ts_ns: i128) -> anyhow::Result<Value> {
    Ok(json!({
        "instrument_key": catalog.catalog_id,
        "venue": catalog.venue_instance_id,
        "instrument_id": catalog.instrument_id,
        "symbol": catalog.display_symbol(),
        "base_asset": catalog.base_asset,
        "quote_asset": catalog.quote_asset,
        "price_tick": catalog.price_tick.map(|value| value.to_string()),
        "size_tick": catalog.size_tick.map(|value| value.to_string()),
        "min_size": catalog.min_size.map(|value| value.to_string()),
        "status": catalog.status,
        "source_raw_json": catalog.source_raw_json.as_ref().map(Value::to_string),
        "updated_ts_ns": i64::try_from(recorded_ts_ns)
            .context("catalog timestamp exceeds Int64")?
    }))
}

fn tick_row(event_id: &str, tick: &BboTick) -> anyhow::Result<Value> {
    let valid = tick.bid.is_some()
        && tick.ask.is_some()
        && !tick.quality.gap
        && !tick.quality.stale
        && !tick.quality.inconsistent;
    Ok(json!({
        "event_id": event_id,
        "instrument_key": tick.instrument.catalog_id,
        "venue": tick.instrument.venue_instance_id,
        "instrument_id": tick.instrument.instrument_id,
        "recv_ts_ns": i64::try_from(tick.recv_ts_ns).context("tick timestamp exceeds Int64")?,
        "exchange_ts_ms": tick.exchange_ts_ms,
        "sequence": tick.sequence.map(|value| value.to_string()),
        "source": source_name(tick.source),
        "bid_price": tick.bid.as_ref().map(|level| level.price.to_string()),
        "bid_size": tick.bid.as_ref().map(|level| level.size.to_string()),
        "bid_order_count": tick.bid.as_ref().and_then(|level| level.order_count),
        "ask_price": tick.ask.as_ref().map(|level| level.price.to_string()),
        "ask_size": tick.ask.as_ref().map(|level| level.size.to_string()),
        "ask_order_count": tick.ask.as_ref().and_then(|level| level.order_count),
        "spread": tick.spread.map(|value| value.to_string()),
        "mid": tick.mid.map(|value| value.to_string()),
        "valid": valid,
        "quality_gap": tick.quality.gap,
        "quality_stale": tick.quality.stale,
        "quality_inconsistent": tick.quality.inconsistent,
        "quality_note": tick.quality.note
    }))
}

fn source_name(source: SourceKind) -> &'static str {
    match source {
        SourceKind::Bbo => "bbo",
        SourceKind::Ticker => "ticker",
        SourceKind::L2Book => "l2_book",
    }
}

fn resolution(range_ms: i64) -> (&'static str, i64) {
    const MINUTE: i64 = 60_000;
    const HOUR: i64 = 60 * MINUTE;
    const DAY: i64 = 24 * HOUR;
    if range_ms <= 15 * MINUTE {
        ("bbo_state_1s", 1_000)
    } else if range_ms <= 12 * HOUR {
        ("bbo_state_1m", MINUTE)
    } else if range_ms <= 3 * DAY {
        ("bbo_state_5m", 5 * MINUTE)
    } else if range_ms <= 10 * DAY {
        ("bbo_state_15m", 15 * MINUTE)
    } else {
        ("bbo_state_1h", HOUR)
    }
}

fn complete_bucket_bounds(from_ms: i64, to_ms: i64, resolution_ms: i64) -> (i64, i64, i64) {
    let from_floor = from_ms.div_euclid(resolution_ms) * resolution_ms;
    let range_from = if from_floor == from_ms {
        from_floor
    } else {
        from_floor + resolution_ms
    };
    let range_before = to_ms.div_euclid(resolution_ms) * resolution_ms;
    (from_floor, range_from, range_before)
}

#[derive(Debug, Clone, Copy)]
struct HistoryWindow {
    from_ms: i64,
    to_ms: i64,
    resolution_ms: i64,
}

fn align_history(
    leg_a: &str,
    leg_b: &str,
    venue_a: &str,
    venue_b: &str,
    window: HistoryWindow,
    seed: Vec<HistoryRow>,
    range: Vec<HistoryRow>,
    resets: Vec<VenueResetRow>,
    a_rate: f64,
    b_rate: f64,
) -> anyhow::Result<Vec<SpreadPoint>> {
    let mut state_a = seed.iter().find(|row| row.instrument_key == leg_a).cloned();
    let mut state_b = seed.iter().find(|row| row.instrument_key == leg_b).cloned();
    let mut by_bucket: BTreeMap<i64, Vec<HistoryRow>> = BTreeMap::new();
    for row in range {
        by_bucket.entry(row.bucket_ms).or_default().push(row);
    }
    let mut resets = resets.into_iter().peekable();
    let mut latest_reset_a_ns = None;
    let mut latest_reset_b_ns = None;

    let mut bucket = window.from_ms.div_euclid(window.resolution_ms) * window.resolution_ms;
    let mut points = Vec::new();
    while bucket < window.to_ms {
        if let Some(rows) = by_bucket.remove(&bucket) {
            for row in rows {
                if row.instrument_key == leg_a {
                    state_a = Some(row);
                } else if row.instrument_key == leg_b {
                    state_b = Some(row);
                }
            }
        }
        let point_ms = (bucket + window.resolution_ms).min(window.to_ms);
        while resets
            .peek()
            .is_some_and(|reset| reset.recv_ts_ns / 1_000_000 <= point_ms)
        {
            let reset = resets.next().expect("peeked reset must exist");
            if reset.venue == venue_a {
                latest_reset_a_ns = Some(reset.recv_ts_ns);
            }
            if reset.venue == venue_b {
                latest_reset_b_ns = Some(reset.recv_ts_ns);
            }
        }
        if latest_reset_a_ns.is_some_and(|reset_ns| {
            state_a
                .as_ref()
                .is_some_and(|state| state.recv_ts_ns <= reset_ns)
        }) {
            state_a = None;
        }
        if latest_reset_b_ns.is_some_and(|reset_ns| {
            state_b
                .as_ref()
                .is_some_and(|state| state.recv_ts_ns <= reset_ns)
        }) {
            state_b = None;
        }
        if point_ms >= window.from_ms
            && let (Some(a), Some(b)) = (&state_a, &state_b)
        {
            points.push(spread_from_history(a, b, point_ms, a_rate, b_rate)?);
        }
        bucket += window.resolution_ms;
    }
    Ok(points)
}

fn spread_from_history(
    first: &HistoryRow,
    second: &HistoryRow,
    ts_ms: i64,
    a_rate: f64,
    b_rate: f64,
) -> anyhow::Result<SpreadPoint> {
    let a_bid = parse_decimal(&first.bid_price)? * a_rate;
    let a_ask = parse_decimal(&first.ask_price)? * a_rate;
    let b_bid = parse_decimal(&second.bid_price)? * b_rate;
    let b_ask = parse_decimal(&second.ask_price)? * b_rate;
    let a_to_b = a_bid - b_ask;
    let b_to_a = b_bid - a_ask;
    Ok(SpreadPoint {
        ts_ms,
        a_state_ts_ms: first.recv_ts_ns / 1_000_000,
        b_state_ts_ms: second.recv_ts_ns / 1_000_000,
        a_bid,
        a_ask,
        b_bid,
        b_ask,
        a_to_b,
        b_to_a,
        a_to_b_bp: a_to_b / b_ask * 10_000.0,
        b_to_a_bp: b_to_a / a_ask * 10_000.0,
    })
}

fn parse_decimal(value: &str) -> anyhow::Result<f64> {
    value
        .parse()
        .with_context(|| format!("parse ClickHouse decimal `{value}`"))
}

fn validate_identifier(value: &str) -> anyhow::Result<()> {
    let mut chars = value.chars();
    let valid_start = chars
        .next()
        .is_some_and(|char| char == '_' || char.is_ascii_alphabetic());
    if !valid_start || !chars.all(|char| char == '_' || char.is_ascii_alphanumeric()) {
        bail!("unsafe ClickHouse identifier `{value}`");
    }
    Ok(())
}

fn quote_identifier(value: &str) -> String {
    format!("`{value}`")
}

fn quote_string(value: &str) -> String {
    format!("'{}'", value.replace('\\', "\\\\").replace('\'', "\\'"))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tempfile::tempdir;
    use tokio::{sync::watch, time};

    use crate::{domain::MarketEvent, wal::DurableEventLog};

    use super::{
        HistoryRow, HistoryWindow, VenueResetRow, align_history, complete_bucket_bounds,
        next_projector_batch, resolution,
    };

    #[test]
    fn chooses_bounded_history_resolutions() {
        assert_eq!(resolution(15 * 60_000), ("bbo_state_1s", 1_000));
        assert_eq!(resolution(24 * 60 * 60_000), ("bbo_state_5m", 300_000));
        assert_eq!(
            resolution(31 * 24 * 60 * 60_000),
            ("bbo_state_1h", 3_600_000)
        );
    }

    #[test]
    fn decodes_clickhouse_quoted_int64_fields() {
        let row: HistoryRow = serde_json::from_str(
            r#"{
                "instrumentKey":"lighter:1:test",
                "bucketMs":"1785042197000",
                "recvTsNs":"1785042197203406000",
                "bidPrice":"64462.4",
                "askPrice":"64465.9"
            }"#,
        )
        .unwrap();
        assert_eq!(row.bucket_ms, 1_785_042_197_000);
        assert_eq!(row.recv_ts_ns, 1_785_042_197_203_406_000);
    }

    #[test]
    fn queries_only_complete_history_buckets() {
        assert_eq!(
            complete_bucket_bounds(1_250, 4_750, 1_000),
            (1_000, 2_000, 4_000)
        );
        assert_eq!(
            complete_bucket_bounds(2_000, 5_000, 1_000),
            (2_000, 2_000, 5_000)
        );
    }

    #[test]
    fn carries_last_bbo_across_quiet_history_without_a_time_expiry() {
        let points = align_history(
            "venue-a:btc",
            "venue-b:btc",
            "venue-a",
            "venue-b",
            HistoryWindow {
                from_ms: 0,
                to_ms: 180_000,
                resolution_ms: 60_000,
            },
            vec![
                history_row("venue-a:btc", 0, 0, "100", "101"),
                history_row("venue-b:btc", 0, 0, "102", "103"),
            ],
            vec![],
            vec![],
            1.0,
            1.0,
        )
        .unwrap();

        assert_eq!(points.len(), 3);
        assert_eq!(points[2].a_state_ts_ms, 0);
        assert_eq!(points[2].b_state_ts_ms, 0);
    }

    #[test]
    fn venue_reset_invalidates_carried_state_until_a_new_bbo_arrives() {
        let points = align_history(
            "venue-a:btc",
            "venue-b:btc",
            "venue-a",
            "venue-b",
            HistoryWindow {
                from_ms: 0,
                to_ms: 180_000,
                resolution_ms: 60_000,
            },
            vec![
                history_row("venue-a:btc", 0, 0, "100", "101"),
                history_row("venue-b:btc", 0, 0, "102", "103"),
            ],
            vec![history_row("venue-a:btc", 120_000, 130_000, "104", "105")],
            vec![VenueResetRow {
                venue: "venue-a".to_string(),
                recv_ts_ns: 90_000_000_000,
            }],
            1.0,
            1.0,
        )
        .unwrap();

        assert_eq!(
            points.iter().map(|point| point.ts_ms).collect::<Vec<_>>(),
            vec![60_000, 180_000]
        );
        assert_eq!(points[1].a_state_ts_ms, 130_000);
    }

    #[tokio::test]
    async fn projector_flushes_immediately_when_batch_is_full() {
        let directory = tempdir().unwrap();
        let wal = DurableEventLog::open(directory.path().join("wal.sqlite3")).unwrap();
        wal.append_batch(&[
            venue_reset("lighter"),
            venue_reset("hyperliquid"),
            venue_reset("perpl"),
        ])
        .unwrap();
        let (_shutdown_tx, mut shutdown) = watch::channel(false);

        let rows = time::timeout(
            Duration::from_millis(200),
            next_projector_batch(&wal, 3, Duration::from_secs(5), &mut shutdown),
        )
        .await
        .unwrap()
        .unwrap()
        .unwrap();

        assert_eq!(rows.len(), 3);
    }

    #[tokio::test]
    async fn projector_collects_new_rows_during_linger_window() {
        let directory = tempdir().unwrap();
        let wal = DurableEventLog::open(directory.path().join("wal.sqlite3")).unwrap();
        wal.append(venue_reset("lighter")).unwrap();
        let writer = wal.clone();
        let (_shutdown_tx, mut shutdown) = watch::channel(false);

        let append = tokio::spawn(async move {
            time::sleep(Duration::from_millis(20)).await;
            writer.append(venue_reset("hyperliquid")).unwrap();
        });
        let rows = time::timeout(
            Duration::from_millis(500),
            next_projector_batch(&wal, 2, Duration::from_secs(5), &mut shutdown),
        )
        .await
        .unwrap()
        .unwrap()
        .unwrap();
        append.await.unwrap();

        assert_eq!(rows.len(), 2);
    }

    #[tokio::test]
    async fn projector_flushes_partial_batch_after_linger_window() {
        let directory = tempdir().unwrap();
        let wal = DurableEventLog::open(directory.path().join("wal.sqlite3")).unwrap();
        wal.append(venue_reset("lighter")).unwrap();
        let (_shutdown_tx, mut shutdown) = watch::channel(false);

        let rows = time::timeout(
            Duration::from_millis(500),
            next_projector_batch(&wal, 5_000, Duration::from_millis(20), &mut shutdown),
        )
        .await
        .unwrap()
        .unwrap()
        .unwrap();

        assert_eq!(rows.len(), 1);
    }

    #[tokio::test]
    async fn projector_stops_waiting_when_shutdown_is_requested() {
        let directory = tempdir().unwrap();
        let wal = DurableEventLog::open(directory.path().join("wal.sqlite3")).unwrap();
        let (shutdown_tx, mut shutdown) = watch::channel(false);

        let stop = tokio::spawn(async move {
            time::sleep(Duration::from_millis(20)).await;
            shutdown_tx.send(true).unwrap();
        });
        let rows = time::timeout(
            Duration::from_millis(500),
            next_projector_batch(&wal, 5_000, Duration::from_secs(5), &mut shutdown),
        )
        .await
        .unwrap()
        .unwrap();
        stop.await.unwrap();

        assert!(rows.is_none());
    }

    fn venue_reset(venue: &str) -> MarketEvent {
        MarketEvent::VenueReset {
            venue_instance_id: venue.to_string(),
        }
    }

    fn history_row(
        instrument_key: &str,
        bucket_ms: i64,
        recv_ms: i64,
        bid_price: &str,
        ask_price: &str,
    ) -> HistoryRow {
        HistoryRow {
            instrument_key: instrument_key.to_string(),
            bucket_ms,
            recv_ts_ns: recv_ms * 1_000_000,
            bid_price: bid_price.to_string(),
            ask_price: ask_price.to_string(),
        }
    }
}
