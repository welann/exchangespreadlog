import assert from 'node:assert/strict';
import test from 'node:test';

import {
  buildBucketSnapshotSpreadPoints,
  buildEventSpreadPoints
} from '../src/lib/server/spread-state.ts';

function tick(
  side,
  tsMs,
  bid,
  ask,
  bidSize = 1,
  askSize = 1
) {
  return {
    side,
    tsMs,
    tsNs: String(tsMs * 1_000_000),
    bid,
    ask,
    bidSize,
    askSize,
    bidSizeText: String(bidSize),
    askSizeText: String(askSize),
    bidOrderCount: null,
    askOrderCount: null,
    mid: (bid + ask) / 2
  };
}

test('carries the quiet venue book forward when the other venue updates', () => {
  const seedRows = [
    tick('a', 1_000, 100, 101),
    tick('b', 1_000, 102, 103)
  ];
  const points = buildEventSpreadPoints(
    seedRows,
    [tick('b', 120_000, 104, 105)],
    1,
    1
  );

  assert.equal(points.length, 1);
  assert.equal(points[0].aBid, 100);
  assert.equal(points[0].aAsk, 101);
  assert.equal(points[0].bBid, 104);
  assert.equal(points[0].bAsk, 105);
  assert.equal(points[0].bToA, 3);
  assert.equal(points[0].id, 'raw:b:120000000000');
});

test('expires a carried book after 120 seconds', () => {
  const seedRows = [
    tick('a', 1_000, 100, 101),
    tick('b', 1_000, 102, 103)
  ];
  const points = buildEventSpreadPoints(
    seedRows,
    [tick('b', 121_001, 104, 105)],
    1,
    1
  );
  assert.deepEqual(points, []);
});

test('materializes fresh seed books at bucket boundaries and then expires them', () => {
  const dayMs = 24 * 60 * 60 * 1_000;
  const seedRows = [
    tick('a', dayMs - 1_000, 100, 101),
    tick('b', dayMs - 1_000, 102, 103)
  ];

  const points = buildBucketSnapshotSpreadPoints(
    seedRows,
    [],
    dayMs,
    dayMs + 3 * 60_000,
    60,
    1,
    1
  );

  assert.equal(points.length, 1);
  assert.deepEqual(
    points.map((point) => [point.aBid, point.aAsk, point.bBid, point.bAsk]),
    [
      [100, 101, 102, 103]
    ]
  );
});

test('does not calculate a spread until both venue states are known', () => {
  const points = buildEventSpreadPoints([], [tick('a', 1_000, 100, 101)], 1, 1);
  assert.deepEqual(points, []);
});
