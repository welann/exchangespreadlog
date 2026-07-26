-- Candle schema v2 for ClickHouse 24.10.
--
-- Required substitutions:
--   {database}             e.g. zeabur
--   {table}                e.g. bbo_ticks
--   {cutover_recv_ts_ns}   activation watermark scheduled after this DDL is
--                          expected to finish (or captured while writes pause)
--
-- For an online rollout, choose a watermark several minutes in the future,
-- finish every view before that watermark, then wait for it to pass before
-- starting 002_backfill_tick_candles.sql. The live raw-table views accept only
-- rows newer than the watermark, while backfill accepts only rows at or below
-- it. This prevents both DDL-window gaps and duplicate aggregate states.

CREATE TABLE IF NOT EXISTS {database}.{table}_candle_1s
(
    venue_instance_id LowCardinality(String),
    instrument_id String,
    bucket_time DateTime64(3, 'UTC'),
    open_mid AggregateFunction(argMin, Float64, Int64),
    high_mid AggregateFunction(max, Float64),
    low_mid AggregateFunction(min, Float64),
    close_mid AggregateFunction(argMax, Float64, Int64),
    final_book AggregateFunction(
        argMax,
        Tuple(
            Nullable(Float64),
            Nullable(Float64),
            Nullable(Float64),
            Nullable(Float64),
            Nullable(UInt32),
            Nullable(UInt32),
            Float64,
            Int64
        ),
        Int64
    ),
    tick_count AggregateFunction(sum, UInt64)
)
ENGINE = AggregatingMergeTree
PARTITION BY toDate(bucket_time)
ORDER BY (venue_instance_id, instrument_id, bucket_time)
TTL toDateTime(bucket_time, 'UTC') + INTERVAL 31 DAY DELETE
SETTINGS index_granularity = 8192;

CREATE TABLE IF NOT EXISTS {database}.{table}_candle_1m AS {database}.{table}_candle_1s
ENGINE = AggregatingMergeTree
PARTITION BY toDate(bucket_time)
ORDER BY (venue_instance_id, instrument_id, bucket_time)
TTL toDateTime(bucket_time, 'UTC') + INTERVAL 31 DAY DELETE
SETTINGS index_granularity = 8192;

CREATE TABLE IF NOT EXISTS {database}.{table}_candle_5m AS {database}.{table}_candle_1s
ENGINE = AggregatingMergeTree
PARTITION BY toDate(bucket_time)
ORDER BY (venue_instance_id, instrument_id, bucket_time)
TTL toDateTime(bucket_time, 'UTC') + INTERVAL 31 DAY DELETE
SETTINGS index_granularity = 8192;

CREATE TABLE IF NOT EXISTS {database}.{table}_candle_15m AS {database}.{table}_candle_1s
ENGINE = AggregatingMergeTree
PARTITION BY toDate(bucket_time)
ORDER BY (venue_instance_id, instrument_id, bucket_time)
TTL toDateTime(bucket_time, 'UTC') + INTERVAL 31 DAY DELETE
SETTINGS index_granularity = 8192;

CREATE TABLE IF NOT EXISTS {database}.{table}_candle_1h AS {database}.{table}_candle_1s
ENGINE = AggregatingMergeTree
PARTITION BY toDate(bucket_time)
ORDER BY (venue_instance_id, instrument_id, bucket_time)
TTL toDateTime(bucket_time, 'UTC') + INTERVAL 31 DAY DELETE
SETTINGS index_granularity = 8192;

CREATE MATERIALIZED VIEW IF NOT EXISTS {database}.{table}_candle_1m_mv
TO {database}.{table}_candle_1m
AS SELECT
    venue_instance_id,
    instrument_id,
    toStartOfMinute(bucket_time) AS bucket_time,
    argMinMergeState(open_mid) AS open_mid,
    maxMergeState(high_mid) AS high_mid,
    minMergeState(low_mid) AS low_mid,
    argMaxMergeState(close_mid) AS close_mid,
    argMaxMergeState(final_book) AS final_book,
    sumMergeState(tick_count) AS tick_count
FROM {database}.{table}_candle_1s
GROUP BY venue_instance_id, instrument_id, toStartOfMinute(bucket_time)
SETTINGS prefer_column_name_to_alias = 1;

CREATE MATERIALIZED VIEW IF NOT EXISTS {database}.{table}_candle_5m_mv
TO {database}.{table}_candle_5m
AS SELECT
    venue_instance_id,
    instrument_id,
    toStartOfFiveMinutes(bucket_time) AS bucket_time,
    argMinMergeState(open_mid) AS open_mid,
    maxMergeState(high_mid) AS high_mid,
    minMergeState(low_mid) AS low_mid,
    argMaxMergeState(close_mid) AS close_mid,
    argMaxMergeState(final_book) AS final_book,
    sumMergeState(tick_count) AS tick_count
FROM {database}.{table}_candle_1m
GROUP BY venue_instance_id, instrument_id, toStartOfFiveMinutes(bucket_time)
SETTINGS prefer_column_name_to_alias = 1;

CREATE MATERIALIZED VIEW IF NOT EXISTS {database}.{table}_candle_15m_mv
TO {database}.{table}_candle_15m
AS SELECT
    venue_instance_id,
    instrument_id,
    toStartOfFifteenMinutes(bucket_time) AS bucket_time,
    argMinMergeState(open_mid) AS open_mid,
    maxMergeState(high_mid) AS high_mid,
    minMergeState(low_mid) AS low_mid,
    argMaxMergeState(close_mid) AS close_mid,
    argMaxMergeState(final_book) AS final_book,
    sumMergeState(tick_count) AS tick_count
FROM {database}.{table}_candle_5m
GROUP BY venue_instance_id, instrument_id, toStartOfFifteenMinutes(bucket_time)
SETTINGS prefer_column_name_to_alias = 1;

CREATE MATERIALIZED VIEW IF NOT EXISTS {database}.{table}_candle_1h_mv
TO {database}.{table}_candle_1h
AS SELECT
    venue_instance_id,
    instrument_id,
    toStartOfHour(bucket_time) AS bucket_time,
    argMinMergeState(open_mid) AS open_mid,
    maxMergeState(high_mid) AS high_mid,
    minMergeState(low_mid) AS low_mid,
    argMaxMergeState(close_mid) AS close_mid,
    argMaxMergeState(final_book) AS final_book,
    sumMergeState(tick_count) AS tick_count
FROM {database}.{table}_candle_15m
GROUP BY venue_instance_id, instrument_id, toStartOfHour(bucket_time)
SETTINGS prefer_column_name_to_alias = 1;

CREATE MATERIALIZED VIEW IF NOT EXISTS {database}.{table}_candle_1s_mv
TO {database}.{table}_candle_1s
AS SELECT
    venue_instance_id,
    instrument_id,
    toStartOfSecond(recv_time) AS bucket_time,
    argMinState(assumeNotNull((bid_price + ask_price) / 2), recv_ts_ns) AS open_mid,
    maxState(assumeNotNull((bid_price + ask_price) / 2)) AS high_mid,
    minState(assumeNotNull((bid_price + ask_price) / 2)) AS low_mid,
    argMaxState(assumeNotNull((bid_price + ask_price) / 2), recv_ts_ns) AS close_mid,
    argMaxState(
        tuple(
            bid_price,
            ask_price,
            bid_size,
            ask_size,
            bid_order_count,
            ask_order_count,
            assumeNotNull((bid_price + ask_price) / 2),
            recv_ts_ns
        ),
        recv_ts_ns
    ) AS final_book,
    sumState(toUInt64(1)) AS tick_count
FROM {database}.{table}
WHERE recv_ts_ns > {cutover_recv_ts_ns}
  AND bid_price IS NOT NULL
  AND ask_price IS NOT NULL
  AND bid_price > 0
  AND ask_price > 0
  AND bid_price <= ask_price
  AND (bid_size IS NULL OR bid_size > 0)
  AND (ask_size IS NULL OR ask_size > 0)
  AND quality_gap = false
  AND quality_stale = false
  AND quality_inconsistent = false
  AND venue_instance_id != ''
  AND instrument_id != ''
GROUP BY venue_instance_id, instrument_id, bucket_time;

CREATE TABLE IF NOT EXISTS {database}.{table}_latest_valid
(
    venue_instance_id LowCardinality(String),
    instrument_id String,
    latest_recv_time AggregateFunction(max, DateTime64(9, 'UTC')),
    valid_tick_count AggregateFunction(sum, UInt64)
)
ENGINE = AggregatingMergeTree
ORDER BY (venue_instance_id, instrument_id);

CREATE MATERIALIZED VIEW IF NOT EXISTS {database}.{table}_latest_valid_mv
TO {database}.{table}_latest_valid
AS SELECT
    venue_instance_id,
    instrument_id,
    maxState(recv_time) AS latest_recv_time,
    sumState(toUInt64(1)) AS valid_tick_count
FROM {database}.{table}
WHERE recv_ts_ns > {cutover_recv_ts_ns}
  AND bid_price IS NOT NULL
  AND ask_price IS NOT NULL
  AND bid_price > 0
  AND ask_price > 0
  AND bid_price <= ask_price
  AND (bid_size IS NULL OR bid_size > 0)
  AND (ask_size IS NULL OR ask_size > 0)
  AND quality_gap = false
  AND quality_stale = false
  AND quality_inconsistent = false
  AND venue_instance_id != ''
  AND instrument_id != ''
GROUP BY venue_instance_id, instrument_id;

CREATE TABLE IF NOT EXISTS {database}.{table}_candle_status
(
    schema_version UInt16,
    ready Bool,
    coverage_from DateTime64(3, 'UTC'),
    coverage_to DateTime64(3, 'UTC'),
    cutover_recv_ts_ns Int64,
    message String,
    updated_at DateTime64(3, 'UTC')
)
ENGINE = ReplacingMergeTree(updated_at)
ORDER BY schema_version;

INSERT INTO {database}.{table}_candle_status
VALUES
(
    2,
    false,
    fromUnixTimestamp64Milli(0),
    fromUnixTimestamp64Milli(0),
    {cutover_recv_ts_ns},
    'schema-created-backfill-required',
    now64(3)
);
