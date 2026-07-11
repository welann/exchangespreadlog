-- Use this only when an existing bbo_ticks table is still ordered by
-- (venue, market_id, recv_time). Newly created tables already use the
-- storage identity as their sorting key.
ALTER TABLE zeabur.bbo_ticks
    ADD PROJECTION IF NOT EXISTS by_storage_identity_query
    (
        SELECT
            venue_instance_id,
            instrument_id,
            recv_time,
            recv_ts_ns,
            bid_price,
            ask_price,
            bid_size,
            ask_size,
            bid_size_text,
            ask_size_text,
            bid_order_count,
            ask_order_count,
            mid
        ORDER BY (venue_instance_id, instrument_id, recv_time)
    );

ALTER TABLE zeabur.bbo_ticks
    MATERIALIZE PROJECTION by_storage_identity_query
    SETTINGS mutations_sync = 0;

-- Keep the exact per-instrument counts used by /api/markets without scanning
-- every tick for each page load.
ALTER TABLE zeabur.bbo_ticks
    ADD PROJECTION IF NOT EXISTS instrument_tick_stats
    (
        SELECT
            venue_instance_id,
            instrument_id,
            max(recv_time) AS latest_recv_time,
            count() AS tick_count
        GROUP BY venue_instance_id, instrument_id
    );

ALTER TABLE zeabur.bbo_ticks
    MATERIALIZE PROJECTION instrument_tick_stats
    SETTINGS mutations_sync = 0;
