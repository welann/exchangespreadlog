import type { Instrument, SpreadPoint } from '$lib/types';
import {
  convertCandleRowsToSpreadPoints,
  type RawCandleRow
} from '$lib/spread/candle-conversion';
import { configuredTable, numericLiteral, queryClickHouse } from './clickhouse';
import { tickIdentityWhere, type TickSchema } from './tick-schema';

export { convertCandleRowsToSpreadPoints };
export type { RawCandleRow };

// ── Granularity types ──────────────────────────────────────────────────────

export type CandleGranularity = 'raw' | '1s' | '1m' | '5m' | '15m' | '1h';

export interface CandleGranularityConfig {
  granularity: CandleGranularity;
  tableName: string;
  bucketMs: number;
}

// ── Candle query result ───────────────────────────────────────────────────

export interface CandleQueryResult {
  points: SpreadPoint[];
  sourceRows: number;
}

// ── Granularity selection ─────────────────────────────────────────────────

const GRANULARITY_TABLE: Array<{
  maxMs: number;
  granularity: CandleGranularity;
  tableSuffix: string;
  bucketMs: number;
}> = [
  { maxMs: 60_000,         granularity: 'raw', tableSuffix: '',              bucketMs: 0 },
  { maxMs: 10 * 60_000,    granularity: '1s',  tableSuffix: '_candle_1s',   bucketMs: 1_000 },
  { maxMs: 60 * 60_000,    granularity: '1m',  tableSuffix: '_candle_1m',   bucketMs: 60_000 },
  { maxMs: 6 * 3600_000,   granularity: '5m',  tableSuffix: '_candle_5m',   bucketMs: 300_000 },
  { maxMs: 24 * 3600_000,  granularity: '15m', tableSuffix: '_candle_15m',  bucketMs: 900_000 },
  { maxMs: 7 * 86400_000,  granularity: '1h',  tableSuffix: '_candle_1h',   bucketMs: 3600_000 },
  { maxMs: Infinity,       granularity: '1h',  tableSuffix: '_candle_1h',   bucketMs: 3600_000 },
];

/**
 * Select the optimal candle granularity for a given time range.
 *
 * The heuristic targets 100-600 data points across the visible range.
 * Ranges ≤ 1 minute stay on raw tick mode; everything else routes through
 * a pre-aggregated candle table.
 */
export function selectGranularity(rangeMs: number): CandleGranularityConfig {
  for (const entry of GRANULARITY_TABLE) {
    if (rangeMs <= entry.maxMs) {
      if (entry.granularity === 'raw') {
        return { granularity: 'raw', tableName: '', bucketMs: 0 };
      }
      return {
        granularity: entry.granularity,
        tableName: configuredTable(entry.tableSuffix),
        bucketMs: entry.bucketMs,
      };
    }
  }

  // Fallback
  return {
    granularity: '1h',
    tableName: configuredTable('_candle_1h'),
    bucketMs: 3600_000,
  };
}

export function candleGranularityForBucketSeconds(
  bucketSeconds: number
): CandleGranularityConfig | null {
  const entry = GRANULARITY_TABLE.find(
    (candidate) =>
      candidate.granularity !== 'raw' &&
      candidate.bucketMs === Math.trunc(bucketSeconds) * 1000
  );
  if (!entry) return null;
  return {
    granularity: entry.granularity,
    tableName: configuredTable(entry.tableSuffix),
    bucketMs: entry.bucketMs
  };
}

// ── Candle query ──────────────────────────────────────────────────────────

export async function fetchCandleRows(
  tickSchema: TickSchema,
  instrumentA: Instrument,
  instrumentB: Instrument,
  fromMs: number,
  toMs: number,
  candleConfig: CandleGranularityConfig,
): Promise<RawCandleRow[]> {
  const whereA = tickIdentityWhere(tickSchema, instrumentA, 'c');
  const whereB = tickIdentityWhere(tickSchema, instrumentB, 'c');

  const tableName = candleConfig.tableName;
  if (!tableName) {
    throw new Error('Candle table name is required for non-raw granularity');
  }

  const alignedFromMs = Math.ceil(fromMs / candleConfig.bucketMs) * candleConfig.bucketMs;
  const alignedToMs = Math.floor(toMs / candleConfig.bucketMs) * candleConfig.bucketMs;
  if (alignedFromMs >= alignedToMs) return [];

  return queryClickHouse<RawCandleRow>(`
    SELECT
      bucket_time,
      multiIf(${whereA}, 'a', ${whereB}, 'b', '') AS side,
      argMinMerge(open_mid) AS open_mid,
      maxMerge(high_mid) AS high_mid,
      minMerge(low_mid) AS low_mid,
      argMaxMerge(close_mid) AS close_mid,
      tupleElement(argMaxMerge(final_book), 1) AS final_bid_price,
      tupleElement(argMaxMerge(final_book), 3) AS final_bid_size,
      tupleElement(argMaxMerge(final_book), 5) AS final_bid_order_count,
      tupleElement(argMaxMerge(final_book), 2) AS final_ask_price,
      tupleElement(argMaxMerge(final_book), 4) AS final_ask_size,
      tupleElement(argMaxMerge(final_book), 6) AS final_ask_order_count,
      tupleElement(argMaxMerge(final_book), 7) AS final_mid,
      tupleElement(argMaxMerge(final_book), 8) AS final_recv_ts_ns
    FROM ${tableName} AS c
    WHERE (${whereA} OR ${whereB})
      AND bucket_time >= fromUnixTimestamp64Milli(${numericLiteral(alignedFromMs)})
      AND bucket_time < fromUnixTimestamp64Milli(${numericLiteral(alignedToMs)})
    GROUP BY bucket_time, venue_instance_id, instrument_id
    ORDER BY bucket_time ASC, side ASC
    FORMAT JSONEachRow
  `);
}

// ── Candle seed rows (carry-forward state before the visible window) ──────

export async function fetchCandleSeedRows(
  tickSchema: TickSchema,
  instrumentA: Instrument,
  instrumentB: Instrument,
  fromMs: number,
  candleConfig: CandleGranularityConfig,
): Promise<RawCandleRow[]> {
  const whereA = tickIdentityWhere(tickSchema, instrumentA, 'c');
  const whereB = tickIdentityWhere(tickSchema, instrumentB, 'c');
  const seedBeforeMs = Math.ceil(fromMs / candleConfig.bucketMs) * candleConfig.bucketMs;

  return queryClickHouse<RawCandleRow>(`
    SELECT
      bucket_time,
      multiIf(${whereA}, 'a', ${whereB}, 'b', '') AS side,
      argMinMerge(open_mid) AS open_mid,
      maxMerge(high_mid) AS high_mid,
      minMerge(low_mid) AS low_mid,
      argMaxMerge(close_mid) AS close_mid,
      tupleElement(argMaxMerge(final_book), 1) AS final_bid_price,
      tupleElement(argMaxMerge(final_book), 3) AS final_bid_size,
      tupleElement(argMaxMerge(final_book), 5) AS final_bid_order_count,
      tupleElement(argMaxMerge(final_book), 2) AS final_ask_price,
      tupleElement(argMaxMerge(final_book), 4) AS final_ask_size,
      tupleElement(argMaxMerge(final_book), 6) AS final_ask_order_count,
      tupleElement(argMaxMerge(final_book), 7) AS final_mid,
      tupleElement(argMaxMerge(final_book), 8) AS final_recv_ts_ns
    FROM ${candleConfig.tableName} AS c
    WHERE (${whereA} OR ${whereB})
      AND bucket_time < fromUnixTimestamp64Milli(${numericLiteral(seedBeforeMs)})
    GROUP BY bucket_time, venue_instance_id, instrument_id
    ORDER BY bucket_time DESC
    LIMIT 1 BY venue_instance_id, instrument_id
    FORMAT JSONEachRow
  `);
}
