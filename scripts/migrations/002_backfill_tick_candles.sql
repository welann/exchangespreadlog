-- Backfill one bounded time slice after the activation watermark has passed.
-- Run newest day first and never overlap ranges. Inserting into the 1s table
-- automatically feeds all coarser views.
--
-- Required substitutions: {database}, {table}, {from_ms}, {to_ms},
-- {cutover_recv_ts_ns}

INSERT INTO {database}.{table}_candle_1s
SELECT
    venue_instance_id,
    instrument_id,
    toStartOfSecond(recv_time) AS bucket_time,
    argMinState((bid_price + ask_price) / 2, recv_ts_ns) AS open_mid,
    maxState((bid_price + ask_price) / 2) AS high_mid,
    minState((bid_price + ask_price) / 2) AS low_mid,
    argMaxState((bid_price + ask_price) / 2, recv_ts_ns) AS close_mid,
    argMaxState(
        tuple(
            bid_price,
            ask_price,
            bid_size,
            ask_size,
            bid_order_count,
            ask_order_count,
            (bid_price + ask_price) / 2,
            recv_ts_ns
        ),
        recv_ts_ns
    ) AS final_book,
    sumState(toUInt64(1)) AS tick_count
FROM {database}.{table}
WHERE recv_time >= fromUnixTimestamp64Milli({from_ms})
  AND recv_time < fromUnixTimestamp64Milli({to_ms})
  AND recv_ts_ns <= {cutover_recv_ts_ns}
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
GROUP BY venue_instance_id, instrument_id, bucket_time
SETTINGS max_threads = 2, max_insert_threads = 1, max_memory_usage = 2147483648;

INSERT INTO {database}.{table}_latest_valid
SELECT
    venue_instance_id,
    instrument_id,
    maxState(recv_time) AS latest_recv_time,
    sumState(toUInt64(1)) AS valid_tick_count
FROM {database}.{table}
WHERE recv_time >= fromUnixTimestamp64Milli({from_ms})
  AND recv_time < fromUnixTimestamp64Milli({to_ms})
  AND recv_ts_ns <= {cutover_recv_ts_ns}
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
GROUP BY venue_instance_id, instrument_id
SETTINGS max_threads = 2, max_insert_threads = 1, max_memory_usage = 2147483648;
