import assert from 'node:assert/strict';
import test from 'node:test';

import { computeTimeWeightedStats } from '../src/lib/spread/analytics.ts';
import {
  mergeSpreadResponses,
  spreadCacheKey,
  TimedLruCache
} from '../src/lib/spread/cache.ts';

function point(id, tsMs, bp) {
  return {
    id,
    tsMs,
    aStateTsMs: tsMs,
    bStateTsMs: tsMs,
    aBid: 1,
    aAsk: 1,
    aBidSize: null,
    aAskSize: null,
    aBidSizeText: null,
    aAskSizeText: null,
    aBidOrderCount: null,
    aAskOrderCount: null,
    bBid: 1,
    bAsk: 1,
    bBidSize: null,
    bAskSize: null,
    bBidSizeText: null,
    bAskSizeText: null,
    bBidOrderCount: null,
    bAskOrderCount: null,
    aMid: 1,
    bMid: 1,
    aToB: 0,
    bToA: 0,
    aToBBp: bp,
    bToABp: null,
    midDiff: 0
  };
}

function response(points, cursor = null) {
  const instrument = {
    catalogId: 'a',
    venueInstanceId: 'venue',
    instrumentId: 'instrument',
    rawSymbol: 'TEST',
    baseAsset: 'TEST',
    quoteAsset: 'USD',
    status: 'active',
    latestRecvMs: 0,
    label: 'TEST'
  };
  return {
    meta: {
      fromMs: 0,
      toMs: 3_000,
      bucketSeconds: 1,
      granularity: 'bucket',
      requestedPrecision: 'bucket',
      source: 'bucket',
      fallbackReason: null,
      coverage: { fromMs: 0, toMs: 3_000, complete: true },
      nextCursor: cursor,
      sourceRows: points.length,
      bookStatePolicy: 'carry_forward_with_expiry',
      maxStaleMs: 120_000,
      targetQuote: 'USD',
      aRate: 1,
      bRate: 1,
      instrumentA: instrument,
      instrumentB: { ...instrument, catalogId: 'b' }
    },
    points
  };
}

test('computes mean, volatility and positive share by elapsed time', () => {
  const stats = computeTimeWeightedStats(
    [point('a', 0, 10), point('b', 1_000, -10)],
    0,
    3_000
  );
  assert.ok(Math.abs(stats.avg - (-10 / 3)) < 1e-9);
  assert.ok(Math.abs(stats.positiveShare - 1 / 3) < 1e-9);
  assert.equal(stats.coverageShare, 1);
  assert.equal(stats.windowCount, 1);
});

test('excludes time beyond state expiry from coverage', () => {
  const stats = computeTimeWeightedStats([point('a', 0, 10)], 0, 3_000, 1_000);
  assert.equal(stats.coverageShare, 1 / 3);
});

test('LRU cache expires entries and evicts the least recently used key', () => {
  const cache = new TimedLruCache(2, 100);
  cache.set('a', 1, 0);
  cache.set('b', 2, 0);
  assert.equal(cache.get('a', 50), 1);
  cache.set('c', 3, 50);
  assert.equal(cache.get('b', 50), null);
  assert.equal(cache.get('a', 101), null);
});

test('canonical cache keys normalize rate order', () => {
  const common = { catalogA: 'a', catalogB: 'b', fromMs: 0, toMs: 1 };
  assert.equal(
    spreadCacheKey({
      ...common,
      rates: [
        { from: 'USDT', to: 'USD', rate: '1' },
        { from: 'USDC', to: 'USD', rate: '1' }
      ]
    }),
    spreadCacheKey({
      ...common,
      rates: [
        { from: 'usdc', to: 'usd', rate: '1' },
        { from: 'usdt', to: 'usd', rate: '1' }
      ]
    })
  );
});

test('delta merge deduplicates by stable id and preserves base query metadata', () => {
  const base = response([point('a', 0, 1), point('b', 1_000, 2)], 'b');
  const delta = response([point('b', 1_000, 3), point('c', 2_000, 4)], 'c');
  delta.meta.fromMs = 1_000;
  delta.meta.source = 'raw';
  const merged = mergeSpreadResponses(base, delta);
  assert.deepEqual(merged.points.map((item) => [item.id, item.aToBBp]), [
    ['a', 1],
    ['b', 3],
    ['c', 4]
  ]);
  assert.equal(merged.meta.fromMs, 0);
  assert.equal(merged.meta.source, 'bucket');
  assert.equal(merged.meta.nextCursor, 'c');
});
