-- Run only after raw/candle parity checks pass for the declared coverage.
-- Required substitutions: {database}, {table}, {coverage_from_ms},
-- {coverage_to_ms}, {cutover_recv_ts_ns}

INSERT INTO {database}.{table}_candle_status
VALUES
(
    2,
    true,
    fromUnixTimestamp64Milli({coverage_from_ms}),
    fromUnixTimestamp64Milli({coverage_to_ms}),
    {cutover_recv_ts_ns},
    'validated',
    now64(3)
);
