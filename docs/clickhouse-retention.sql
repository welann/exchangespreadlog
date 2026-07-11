-- Destructive cleanup for pre-storage-identity rows. Run only after all
-- writers populate venue_instance_id and instrument_id.
ALTER TABLE zeabur.bbo_ticks
    DELETE WHERE venue_instance_id = '' OR instrument_id = ''
    SETTINGS mutations_sync = 0;

-- ClickHouse 24.10 requires the DateTime64 value to be converted to DateTime
-- before it can be used as a table TTL expression.
ALTER TABLE zeabur.bbo_ticks
    MODIFY TTL toDateTime(recv_time, 'UTC') + INTERVAL 31 DAY DELETE;
