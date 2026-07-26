CREATE TABLE IF NOT EXISTS {database}.instrument_catalog
(
    instrument_key String,
    venue LowCardinality(String),
    instrument_id String,
    symbol String,
    base_asset LowCardinality(String),
    quote_asset LowCardinality(String),
    price_tick Nullable(Decimal128(18)),
    size_tick Nullable(Decimal128(18)),
    min_size Nullable(Decimal128(18)),
    status LowCardinality(String),
    source_raw_json Nullable(String),
    updated_ts_ns Int64,
    updated_at DateTime64(9, 'UTC') MATERIALIZED fromUnixTimestamp64Nano(updated_ts_ns)
)
ENGINE = ReplacingMergeTree(updated_ts_ns)
ORDER BY (venue, instrument_id)
-- migrate:split
CREATE TABLE IF NOT EXISTS {database}.bbo_events
(
    event_id String,
    instrument_key String,
    venue LowCardinality(String),
    instrument_id String,
    recv_ts_ns Int64,
    recv_time DateTime64(9, 'UTC') MATERIALIZED fromUnixTimestamp64Nano(recv_ts_ns),
    exchange_ts_ms Nullable(Int64),
    sequence Nullable(String),
    source LowCardinality(String),
    bid_price Nullable(Decimal128(18)),
    bid_size Nullable(Decimal128(18)),
    bid_order_count Nullable(UInt32),
    ask_price Nullable(Decimal128(18)),
    ask_size Nullable(Decimal128(18)),
    ask_order_count Nullable(UInt32),
    spread Nullable(Decimal128(18)),
    mid Nullable(Decimal128(18)),
    valid Bool,
    quality_gap Bool,
    quality_stale Bool,
    quality_inconsistent Bool,
    quality_note Nullable(String),
    inserted_at DateTime64(3, 'UTC') DEFAULT now64(3)
)
ENGINE = ReplacingMergeTree(inserted_at)
PARTITION BY toDate(recv_time)
ORDER BY (instrument_key, recv_time, event_id)
TTL toDateTime(recv_time, 'UTC') + INTERVAL 6 HOUR DELETE
-- migrate:split
CREATE TABLE IF NOT EXISTS {database}.venue_state_events
(
    event_id String,
    venue LowCardinality(String),
    state LowCardinality(String),
    reason LowCardinality(String),
    recv_ts_ns Int64,
    recv_time DateTime64(9, 'UTC') MATERIALIZED fromUnixTimestamp64Nano(recv_ts_ns)
)
ENGINE = ReplacingMergeTree(recv_ts_ns)
PARTITION BY toDate(recv_time)
ORDER BY (venue, recv_time, event_id)
TTL toDateTime(recv_time, 'UTC') + INTERVAL 31 DAY DELETE
-- migrate:split
CREATE TABLE IF NOT EXISTS {database}.bbo_state_1s
(
    instrument_key String,
    bucket_time DateTime64(3, 'UTC'),
    recv_ts_ns Int64,
    bid_price Decimal128(18),
    bid_size Decimal128(18),
    ask_price Decimal128(18),
    ask_size Decimal128(18)
)
ENGINE = ReplacingMergeTree(recv_ts_ns)
PARTITION BY toDate(bucket_time)
ORDER BY (instrument_key, bucket_time)
TTL toDateTime(bucket_time, 'UTC') + INTERVAL 1 DAY DELETE
-- migrate:split
CREATE TABLE IF NOT EXISTS {database}.bbo_state_1m AS {database}.bbo_state_1s
ENGINE = ReplacingMergeTree(recv_ts_ns)
PARTITION BY toDate(bucket_time)
ORDER BY (instrument_key, bucket_time)
TTL toDateTime(bucket_time, 'UTC') + INTERVAL 1 DAY DELETE
-- migrate:split
CREATE TABLE IF NOT EXISTS {database}.bbo_state_5m AS {database}.bbo_state_1s
ENGINE = ReplacingMergeTree(recv_ts_ns)
PARTITION BY toDate(bucket_time)
ORDER BY (instrument_key, bucket_time)
TTL toDateTime(bucket_time, 'UTC') + INTERVAL 4 DAY DELETE
-- migrate:split
CREATE TABLE IF NOT EXISTS {database}.bbo_state_15m AS {database}.bbo_state_1s
ENGINE = ReplacingMergeTree(recv_ts_ns)
PARTITION BY toDate(bucket_time)
ORDER BY (instrument_key, bucket_time)
TTL toDateTime(bucket_time, 'UTC') + INTERVAL 11 DAY DELETE
-- migrate:split
CREATE TABLE IF NOT EXISTS {database}.bbo_state_1h AS {database}.bbo_state_1s
ENGINE = ReplacingMergeTree(recv_ts_ns)
PARTITION BY toDate(bucket_time)
ORDER BY (instrument_key, bucket_time)
TTL toDateTime(bucket_time, 'UTC') + INTERVAL 35 DAY DELETE
-- migrate:split
CREATE MATERIALIZED VIEW IF NOT EXISTS {database}.bbo_state_1s_mv
TO {database}.bbo_state_1s
AS SELECT
    e.instrument_key AS instrument_key,
    toStartOfInterval(e.recv_time, INTERVAL 1 SECOND) AS bucket_time,
    max(e.recv_ts_ns) AS recv_ts_ns,
    argMax(assumeNotNull(e.bid_price), e.recv_ts_ns) AS bid_price,
    argMax(assumeNotNull(e.bid_size), e.recv_ts_ns) AS bid_size,
    argMax(assumeNotNull(e.ask_price), e.recv_ts_ns) AS ask_price,
    argMax(assumeNotNull(e.ask_size), e.recv_ts_ns) AS ask_size
FROM {database}.bbo_events AS e
WHERE valid
GROUP BY e.instrument_key, bucket_time
-- migrate:split
CREATE MATERIALIZED VIEW IF NOT EXISTS {database}.bbo_state_1m_mv
TO {database}.bbo_state_1m
AS SELECT
    e.instrument_key AS instrument_key,
    toStartOfInterval(e.recv_time, INTERVAL 1 MINUTE) AS bucket_time,
    max(e.recv_ts_ns) AS recv_ts_ns,
    argMax(assumeNotNull(e.bid_price), e.recv_ts_ns) AS bid_price,
    argMax(assumeNotNull(e.bid_size), e.recv_ts_ns) AS bid_size,
    argMax(assumeNotNull(e.ask_price), e.recv_ts_ns) AS ask_price,
    argMax(assumeNotNull(e.ask_size), e.recv_ts_ns) AS ask_size
FROM {database}.bbo_events AS e
WHERE valid
GROUP BY e.instrument_key, bucket_time
-- migrate:split
ALTER TABLE {database}.bbo_events
MODIFY TTL toDateTime(recv_time, 'UTC') + INTERVAL 6 HOUR DELETE
-- migrate:split
ALTER TABLE {database}.bbo_state_1s
MODIFY TTL toDateTime(bucket_time, 'UTC') + INTERVAL 1 DAY DELETE
-- migrate:split
ALTER TABLE {database}.bbo_state_1m
MODIFY TTL toDateTime(bucket_time, 'UTC') + INTERVAL 1 DAY DELETE
-- migrate:split
ALTER TABLE {database}.bbo_state_5m
MODIFY TTL toDateTime(bucket_time, 'UTC') + INTERVAL 4 DAY DELETE
-- migrate:split
ALTER TABLE {database}.bbo_state_15m
MODIFY TTL toDateTime(bucket_time, 'UTC') + INTERVAL 11 DAY DELETE
-- migrate:split
ALTER TABLE {database}.bbo_state_1h
MODIFY TTL toDateTime(bucket_time, 'UTC') + INTERVAL 35 DAY DELETE
-- migrate:split
CREATE MATERIALIZED VIEW IF NOT EXISTS {database}.bbo_state_5m_mv
TO {database}.bbo_state_5m
AS SELECT
    e.instrument_key AS instrument_key,
    toStartOfInterval(e.recv_time, INTERVAL 5 MINUTE) AS bucket_time,
    max(e.recv_ts_ns) AS recv_ts_ns,
    argMax(assumeNotNull(e.bid_price), e.recv_ts_ns) AS bid_price,
    argMax(assumeNotNull(e.bid_size), e.recv_ts_ns) AS bid_size,
    argMax(assumeNotNull(e.ask_price), e.recv_ts_ns) AS ask_price,
    argMax(assumeNotNull(e.ask_size), e.recv_ts_ns) AS ask_size
FROM {database}.bbo_events AS e
WHERE valid
GROUP BY e.instrument_key, bucket_time
-- migrate:split
CREATE MATERIALIZED VIEW IF NOT EXISTS {database}.bbo_state_15m_mv
TO {database}.bbo_state_15m
AS SELECT
    e.instrument_key AS instrument_key,
    toStartOfInterval(e.recv_time, INTERVAL 15 MINUTE) AS bucket_time,
    max(e.recv_ts_ns) AS recv_ts_ns,
    argMax(assumeNotNull(e.bid_price), e.recv_ts_ns) AS bid_price,
    argMax(assumeNotNull(e.bid_size), e.recv_ts_ns) AS bid_size,
    argMax(assumeNotNull(e.ask_price), e.recv_ts_ns) AS ask_price,
    argMax(assumeNotNull(e.ask_size), e.recv_ts_ns) AS ask_size
FROM {database}.bbo_events AS e
WHERE valid
GROUP BY e.instrument_key, bucket_time
-- migrate:split
CREATE MATERIALIZED VIEW IF NOT EXISTS {database}.bbo_state_1h_mv
TO {database}.bbo_state_1h
AS SELECT
    e.instrument_key AS instrument_key,
    toStartOfInterval(e.recv_time, INTERVAL 1 HOUR) AS bucket_time,
    max(e.recv_ts_ns) AS recv_ts_ns,
    argMax(assumeNotNull(e.bid_price), e.recv_ts_ns) AS bid_price,
    argMax(assumeNotNull(e.bid_size), e.recv_ts_ns) AS bid_size,
    argMax(assumeNotNull(e.ask_price), e.recv_ts_ns) AS ask_price,
    argMax(assumeNotNull(e.ask_size), e.recv_ts_ns) AS ask_size
FROM {database}.bbo_events AS e
WHERE valid
GROUP BY e.instrument_key, bucket_time
