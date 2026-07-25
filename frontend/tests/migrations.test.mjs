import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import test from 'node:test';

const migrationUrl = new URL('../../scripts/migrations/001_tick_candles.sql', import.meta.url);
const backfillUrl = new URL(
  '../../scripts/migrations/002_backfill_tick_candles.sql',
  import.meta.url
);

test('candle schema uses mergeable aggregate states for all five layers', async () => {
  const sql = await readFile(migrationUrl, 'utf8');

  for (const interval of ['1s', '1m', '5m', '15m', '1h']) {
    assert.match(sql, new RegExp(`\\{table\\}_candle_${interval}\\b`));
  }
  assert.match(sql, /AggregateFunction\(argMin,/);
  assert.match(sql, /AggregateFunction\(\s*argMax,\s*Tuple\(/);
  assert.match(sql, /argMinMergeState\(open_mid\)/);
  assert.match(sql, /argMaxMergeState\(final_book\)/);
  assert.match(sql, /sumMergeState\(tick_count\)/);
  assert.doesNotMatch(sql, /SimpleAggregateFunction/);
  assert.doesNotMatch(sql, /\bFINAL\b/);
  assert.doesNotMatch(sql, /\btick_table\b/);
});

test('live and backfill paths are separated by the cutover watermark', async () => {
  const [schema, backfill] = await Promise.all([
    readFile(migrationUrl, 'utf8'),
    readFile(backfillUrl, 'utf8')
  ]);

  assert.match(schema, /recv_ts_ns > \{cutover_recv_ts_ns\}/);
  assert.match(backfill, /recv_ts_ns <= \{cutover_recv_ts_ns\}/);
  assert.match(backfill, /max_threads = 2/);
  assert.match(backfill, /max_insert_threads = 1/);
});
