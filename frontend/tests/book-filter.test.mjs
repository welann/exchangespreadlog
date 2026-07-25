import assert from 'node:assert/strict';
import test from 'node:test';

import { validBookWhere } from '../src/lib/server/book-filter.ts';

test('market discovery and spread queries share complete valid-book predicates', () => {
  const sql = validBookWhere({ hasQualityFlags: true }, 'ticks');

  assert.match(sql, /ticks\.bid_price IS NOT NULL/);
  assert.match(sql, /ticks\.ask_price IS NOT NULL/);
  assert.match(sql, /ticks\.bid_price <= ticks\.ask_price/);
  assert.match(sql, /ticks\.bid_size IS NULL OR ticks\.bid_size > 0/);
  assert.match(sql, /ticks\.quality_gap = false/);
  assert.match(sql, /ticks\.quality_stale = false/);
  assert.match(sql, /ticks\.quality_inconsistent = false/);
});

test('legacy schemas omit unavailable quality flags but keep book validation', () => {
  const sql = validBookWhere({ hasQualityFlags: false }, 'legacy');

  assert.match(sql, /legacy\.bid_price > 0/);
  assert.doesNotMatch(sql, /quality_/);
});
