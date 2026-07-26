import assert from 'node:assert/strict';
import test from 'node:test';

import { convertCandleRowsToSpreadPoints } from '../src/lib/spread/candle-conversion.ts';
import { candleRangeIsCovered } from '../src/lib/spread/candle-coverage.ts';

const instrument = {
  catalogId: 'catalog',
  venueInstanceId: 'venue',
  instrumentId: 'instrument',
  rawSymbol: 'TEST',
  baseAsset: 'TEST',
  quoteAsset: 'USD',
  status: 'active',
  latestRecvMs: 0,
  label: 'venue TEST/USD'
};

function row(side, bucketMs, bid, ask, stateMs = bucketMs + 10_000) {
  return {
    bucket_time: new Date(bucketMs).toISOString(),
    side,
    open_mid: (bid + ask) / 2,
    high_mid: (bid + ask) / 2,
    low_mid: (bid + ask) / 2,
    close_mid: (bid + ask) / 2,
    final_bid_price: bid,
    final_bid_size: 1,
    final_bid_order_count: 1,
    final_ask_price: ask,
    final_ask_size: 1,
    final_ask_order_count: 1,
    final_mid: (bid + ask) / 2,
    final_recv_ts_ns: String(stateMs * 1_000_000)
  };
}

test('aligns candle conversion to complete bucket boundaries', () => {
  const rows = [
    row('a', 0, 99, 100),
    row('b', 0, 101, 102),
    row('a', 60_000, 100, 101),
    row('b', 60_000, 102, 103),
    row('a', 120_000, 101, 102),
    row('b', 120_000, 103, 104)
  ];
  const points = convertCandleRowsToSpreadPoints(
    rows,
    30_000,
    180_000,
    60_000,
    instrument,
    instrument,
    1,
    1
  );

  assert.deepEqual(points.map((point) => point.tsMs), [120_000, 180_000]);
  assert.deepEqual(points.map((point) => point.id), [
    'candle:60000:120000',
    'candle:60000:180000'
  ]);
});

test('rejects incomplete candle snapshots instead of converting null to zero', () => {
  const rows = [
    { ...row('a', 60_000, 100, 101), final_bid_price: null },
    row('b', 60_000, 102, 103)
  ];
  const points = convertCandleRowsToSpreadPoints(
    rows,
    60_000,
    180_000,
    60_000,
    instrument,
    instrument,
    1,
    1
  );
  assert.deepEqual(points, []);
});

test('allows a current request while cached live coverage remains fresh', () => {
  const nowMs = 100_000_000;
  const coverage = {
    ready: true,
    coverageFromMs: 0,
    coverageToMs: nowMs - 60_000
  };

  assert.equal(
    candleRangeIsCovered(coverage, nowMs - 86_400_000, nowMs, nowMs, 120_000),
    true
  );
});

test('does not extend stale or historical coverage optimistically', () => {
  const nowMs = 1_000_000;
  assert.equal(
    candleRangeIsCovered(
      {
        ready: true,
        coverageFromMs: 0,
        coverageToMs: nowMs - 181_000
      },
      0,
      nowMs,
      nowMs,
      120_000
    ),
    false
  );
  assert.equal(
    candleRangeIsCovered(
      {
        ready: true,
        coverageFromMs: 500_000,
        coverageToMs: nowMs
      },
      499_999,
      nowMs,
      nowMs,
      120_000
    ),
    false
  );
});
