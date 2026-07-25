-- =============================================================================
-- Migration 001: Pre-aggregated Candlestick Materialized Views
-- =============================================================================
-- Creates 5 layers of candle tables (1s, 1m, 5m, 15m, 1h) with cascading
-- materialized views from the raw tick table. Each table uses
-- AggregatingMergeTree with SimpleAggregateFunction for the aggregate columns.
--
-- The lowest layer (1s) is fed directly from the tick table. Each subsequent
-- layer aggregates from the next-finer layer: 1m ← 1s, 5m ← 1m, etc.
--
-- Query pattern: use `argMinMerge(open_mid)`, `maxMerge(high_mid)`, etc. when
-- reading from these tables, or include the -Merge suffix automatically if
-- using FINAL (not required for most dashboards).
--
-- All times are bucket-aligned (toStartOfSecond, toStartOfMinute, etc.) and
-- stored as DateTime64(3, 'UTC').
-- =============================================================================

-- =============================================================================
-- Candle Table Template
-- =============================================================================
-- Repeated 5 times below for each granularity. Column list is identical.
-- =============================================================================

-- ── Layer 1: 1-second candles (source: tick_table) ────────────────────────

CREATE TABLE IF NOT EXISTS {database}.tick_candle_1s
(
    venue_instance_id LowCardinality(String),
    instrument_id String,
    bucket_time DateTime64(3, 'UTC'),
    -- OHLC based on mid = (bid + ask) / 2
    open_mid SimpleAggregateFunction(argMin, Float64),
    high_mid SimpleAggregateFunction(max, Float64),
    low_mid SimpleAggregateFunction(min, Float64),
    close_mid SimpleAggregateFunction(argMax, Float64),
    -- Final snapshot within bucket (for spread carry-forward)
    final_bid_price SimpleAggregateFunction(argMax, Float64),
    final_bid_size SimpleAggregateFunction(argMax, Float64),
    final_bid_order_count SimpleAggregateFunction(argMax, UInt32),
    final_ask_price SimpleAggregateFunction(argMax, Float64),
    final_ask_size SimpleAggregateFunction(argMax, Float64),
    final_ask_order_count SimpleAggregateFunction(argMax, UInt32),
    -- Metadata
    tick_count SimpleAggregateFunction(sum, UInt32)
)
ENGINE = AggregatingMergeTree()
PARTITION BY toDate(bucket_time)
ORDER BY (venue_instance_id, instrument_id, bucket_time)
SETTINGS index_granularity = 8192;

CREATE MATERIALIZED VIEW IF NOT EXISTS {database}.tick_candle_1s_mv
TO {database}.tick_candle_1s
AS SELECT
    venue_instance_id,
    instrument_id,
    toStartOfSecond(recv_time) AS bucket_time,
    -- OHLC on mid
    argMinState((bid_price + ask_price) / 2, recv_time) AS open_mid,
    maxState((bid_price + ask_price) / 2) AS high_mid,
    minState((bid_price + ask_price) / 2) AS low_mid,
    argMaxState((bid_price + ask_price) / 2, recv_time) AS close_mid,
    -- Final snapshot (last per bucket, ordered by recv_time)
    argMaxState(bid_price, recv_time) AS final_bid_price,
    argMaxState(bid_size, recv_time) AS final_bid_size,
    argMaxState(bid_order_count, recv_time) AS final_bid_order_count,
    argMaxState(ask_price, recv_time) AS final_ask_price,
    argMaxState(ask_size, recv_time) AS final_ask_size,
    argMaxState(ask_order_count, recv_time) AS final_ask_order_count,
    -- Count
    sumState(1) AS tick_count
FROM {database}.tick_table
WHERE bid_price IS NOT NULL AND ask_price IS NOT NULL
  AND bid_price > 0 AND ask_price > 0
  AND bid_price <= ask_price
  AND (bid_size IS NULL OR bid_size > 0)
  AND (ask_size IS NULL OR ask_size > 0)
  AND venue_instance_id != '' AND instrument_id != ''
GROUP BY venue_instance_id, instrument_id, bucket_time;

-- ── Layer 2: 1-minute candles (source: tick_candle_1s) ────────────────────

CREATE TABLE IF NOT EXISTS {database}.tick_candle_1m
(
    venue_instance_id LowCardinality(String),
    instrument_id String,
    bucket_time DateTime64(3, 'UTC'),
    open_mid SimpleAggregateFunction(argMin, Float64),
    high_mid SimpleAggregateFunction(max, Float64),
    low_mid SimpleAggregateFunction(min, Float64),
    close_mid SimpleAggregateFunction(argMax, Float64),
    final_bid_price SimpleAggregateFunction(argMax, Float64),
    final_bid_size SimpleAggregateFunction(argMax, Float64),
    final_bid_order_count SimpleAggregateFunction(argMax, UInt32),
    final_ask_price SimpleAggregateFunction(argMax, Float64),
    final_ask_size SimpleAggregateFunction(argMax, Float64),
    final_ask_order_count SimpleAggregateFunction(argMax, UInt32),
    tick_count SimpleAggregateFunction(sum, UInt32)
)
ENGINE = AggregatingMergeTree()
PARTITION BY toDate(bucket_time)
ORDER BY (venue_instance_id, instrument_id, bucket_time)
SETTINGS index_granularity = 8192;

CREATE MATERIALIZED VIEW IF NOT EXISTS {database}.tick_candle_1m_mv
TO {database}.tick_candle_1m
AS SELECT
    venue_instance_id,
    instrument_id,
    toStartOfMinute(bucket_time) AS bucket_time,
    -- Aggregate OHLC from the 1s layer
    argMinState(argMinMerge(open_mid), bucket_time) AS open_mid,
    maxState(maxMerge(high_mid)) AS high_mid,
    minState(minMerge(low_mid)) AS low_mid,
    argMaxState(argMaxMerge(close_mid), bucket_time) AS close_mid,
    -- Final snapshot: argMax on the already-argMax columns
    argMaxState(argMaxMerge(final_bid_price), bucket_time) AS final_bid_price,
    argMaxState(argMaxMerge(final_bid_size), bucket_time) AS final_bid_size,
    argMaxState(argMaxMerge(final_bid_order_count), bucket_time) AS final_bid_order_count,
    argMaxState(argMaxMerge(final_ask_price), bucket_time) AS final_ask_price,
    argMaxState(argMaxMerge(final_ask_size), bucket_time) AS final_ask_size,
    argMaxState(argMaxMerge(final_ask_order_count), bucket_time) AS final_ask_order_count,
    sumState(sumMerge(tick_count)) AS tick_count
FROM {database}.tick_candle_1s
GROUP BY venue_instance_id, instrument_id, bucket_time;

-- ── Layer 3: 5-minute candles (source: tick_candle_1m) ────────────────────

CREATE TABLE IF NOT EXISTS {database}.tick_candle_5m
(
    venue_instance_id LowCardinality(String),
    instrument_id String,
    bucket_time DateTime64(3, 'UTC'),
    open_mid SimpleAggregateFunction(argMin, Float64),
    high_mid SimpleAggregateFunction(max, Float64),
    low_mid SimpleAggregateFunction(min, Float64),
    close_mid SimpleAggregateFunction(argMax, Float64),
    final_bid_price SimpleAggregateFunction(argMax, Float64),
    final_bid_size SimpleAggregateFunction(argMax, Float64),
    final_bid_order_count SimpleAggregateFunction(argMax, UInt32),
    final_ask_price SimpleAggregateFunction(argMax, Float64),
    final_ask_size SimpleAggregateFunction(argMax, Float64),
    final_ask_order_count SimpleAggregateFunction(argMax, UInt32),
    tick_count SimpleAggregateFunction(sum, UInt32)
)
ENGINE = AggregatingMergeTree()
PARTITION BY toDate(bucket_time)
ORDER BY (venue_instance_id, instrument_id, bucket_time)
SETTINGS index_granularity = 8192;

CREATE MATERIALIZED VIEW IF NOT EXISTS {database}.tick_candle_5m_mv
TO {database}.tick_candle_5m
AS SELECT
    venue_instance_id,
    instrument_id,
    toStartOfFiveMinutes(bucket_time) AS bucket_time,
    argMinState(argMinMerge(open_mid), bucket_time) AS open_mid,
    maxState(maxMerge(high_mid)) AS high_mid,
    minState(minMerge(low_mid)) AS low_mid,
    argMaxState(argMaxMerge(close_mid), bucket_time) AS close_mid,
    argMaxState(argMaxMerge(final_bid_price), bucket_time) AS final_bid_price,
    argMaxState(argMaxMerge(final_bid_size), bucket_time) AS final_bid_size,
    argMaxState(argMaxMerge(final_bid_order_count), bucket_time) AS final_bid_order_count,
    argMaxState(argMaxMerge(final_ask_price), bucket_time) AS final_ask_price,
    argMaxState(argMaxMerge(final_ask_size), bucket_time) AS final_ask_size,
    argMaxState(argMaxMerge(final_ask_order_count), bucket_time) AS final_ask_order_count,
    sumState(sumMerge(tick_count)) AS tick_count
FROM {database}.tick_candle_1m
GROUP BY venue_instance_id, instrument_id, bucket_time;

-- ── Layer 4: 15-minute candles (source: tick_candle_5m) ───────────────────

CREATE TABLE IF NOT EXISTS {database}.tick_candle_15m
(
    venue_instance_id LowCardinality(String),
    instrument_id String,
    bucket_time DateTime64(3, 'UTC'),
    open_mid SimpleAggregateFunction(argMin, Float64),
    high_mid SimpleAggregateFunction(max, Float64),
    low_mid SimpleAggregateFunction(min, Float64),
    close_mid SimpleAggregateFunction(argMax, Float64),
    final_bid_price SimpleAggregateFunction(argMax, Float64),
    final_bid_size SimpleAggregateFunction(argMax, Float64),
    final_bid_order_count SimpleAggregateFunction(argMax, UInt32),
    final_ask_price SimpleAggregateFunction(argMax, Float64),
    final_ask_size SimpleAggregateFunction(argMax, Float64),
    final_ask_order_count SimpleAggregateFunction(argMax, UInt32),
    tick_count SimpleAggregateFunction(sum, UInt32)
)
ENGINE = AggregatingMergeTree()
PARTITION BY toDate(bucket_time)
ORDER BY (venue_instance_id, instrument_id, bucket_time)
SETTINGS index_granularity = 8192;

CREATE MATERIALIZED VIEW IF NOT EXISTS {database}.tick_candle_15m_mv
TO {database}.tick_candle_15m
AS SELECT
    venue_instance_id,
    instrument_id,
    toStartOfFifteenMinutes(bucket_time) AS bucket_time,
    argMinState(argMinMerge(open_mid), bucket_time) AS open_mid,
    maxState(maxMerge(high_mid)) AS high_mid,
    minState(minMerge(low_mid)) AS low_mid,
    argMaxState(argMaxMerge(close_mid), bucket_time) AS close_mid,
    argMaxState(argMaxMerge(final_bid_price), bucket_time) AS final_bid_price,
    argMaxState(argMaxMerge(final_bid_size), bucket_time) AS final_bid_size,
    argMaxState(argMaxMerge(final_bid_order_count), bucket_time) AS final_bid_order_count,
    argMaxState(argMaxMerge(final_ask_price), bucket_time) AS final_ask_price,
    argMaxState(argMaxMerge(final_ask_size), bucket_time) AS final_ask_size,
    argMaxState(argMaxMerge(final_ask_order_count), bucket_time) AS final_ask_order_count,
    sumState(sumMerge(tick_count)) AS tick_count
FROM {database}.tick_candle_5m
GROUP BY venue_instance_id, instrument_id, bucket_time;

-- ── Layer 5: 1-hour candles (source: tick_candle_15m) ─────────────────────

CREATE TABLE IF NOT EXISTS {database}.tick_candle_1h
(
    venue_instance_id LowCardinality(String),
    instrument_id String,
    bucket_time DateTime64(3, 'UTC'),
    open_mid SimpleAggregateFunction(argMin, Float64),
    high_mid SimpleAggregateFunction(max, Float64),
    low_mid SimpleAggregateFunction(min, Float64),
    close_mid SimpleAggregateFunction(argMax, Float64),
    final_bid_price SimpleAggregateFunction(argMax, Float64),
    final_bid_size SimpleAggregateFunction(argMax, Float64),
    final_bid_order_count SimpleAggregateFunction(argMax, UInt32),
    final_ask_price SimpleAggregateFunction(argMax, Float64),
    final_ask_size SimpleAggregateFunction(argMax, Float64),
    final_ask_order_count SimpleAggregateFunction(argMax, UInt32),
    tick_count SimpleAggregateFunction(sum, UInt32)
)
ENGINE = AggregatingMergeTree()
PARTITION BY toDate(bucket_time)
ORDER BY (venue_instance_id, instrument_id, bucket_time)
SETTINGS index_granularity = 8192;

CREATE MATERIALIZED VIEW IF NOT EXISTS {database}.tick_candle_1h_mv
TO {database}.tick_candle_1h
AS SELECT
    venue_instance_id,
    instrument_id,
    toStartOfHour(bucket_time) AS bucket_time,
    argMinState(argMinMerge(open_mid), bucket_time) AS open_mid,
    maxState(maxMerge(high_mid)) AS high_mid,
    minState(minMerge(low_mid)) AS low_mid,
    argMaxState(argMaxMerge(close_mid), bucket_time) AS close_mid,
    argMaxState(argMaxMerge(final_bid_price), bucket_time) AS final_bid_price,
    argMaxState(argMaxMerge(final_bid_size), bucket_time) AS final_bid_size,
    argMaxState(argMaxMerge(final_bid_order_count), bucket_time) AS final_bid_order_count,
    argMaxState(argMaxMerge(final_ask_price), bucket_time) AS final_ask_price,
    argMaxState(argMaxMerge(final_ask_size), bucket_time) AS final_ask_size,
    argMaxState(argMaxMerge(final_ask_order_count), bucket_time) AS final_ask_order_count,
    sumState(sumMerge(tick_count)) AS tick_count
FROM {database}.tick_candle_15m
GROUP BY venue_instance_id, instrument_id, bucket_time;
