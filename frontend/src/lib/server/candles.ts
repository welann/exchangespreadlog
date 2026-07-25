import type { Instrument, SpreadPoint } from '$lib/types';
import { clickHouseConfig, numericLiteral, queryClickHouse, tickTable } from './clickhouse';
import { tickIdentityWhere, type TickSchema } from './tick-schema';
import { validBookWhere } from './book-filter';

// ── Granularity types ──────────────────────────────────────────────────────

export type CandleGranularity = 'raw' | '1s' | '1m' | '5m' | '15m' | '1h';

export interface CandleGranularityConfig {
  granularity: CandleGranularity;
  tableName: string;
  bucketMs: number;
}

// ── Raw candle row from ClickHouse ─────────────────────────────────────────

/** Shape of a single row returned by a candle table query.
 *  Note: values are already merged by the -Merge suffix applied via FINAL or
 *  explicit merge functions. The query uses FINAL for simplicity. */
export interface RawCandleRow {
  bucket_time: string;
  side: string;
  open_mid: number | null;
  high_mid: number | null;
  low_mid: number | null;
  close_mid: number | null;
  final_bid_price: number | null;
  final_bid_size: number | null;
  final_bid_order_count: number | null;
  final_ask_price: number | null;
  final_ask_size: number | null;
  final_ask_order_count: number | null;
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
      const config = clickHouseConfig();
      return {
        granularity: entry.granularity,
        tableName: `${config.database}.${config.table}${entry.tableSuffix}`,
        bucketMs: entry.bucketMs,
      };
    }
  }

  // Fallback
  const config = clickHouseConfig();
  return {
    granularity: '1h',
    tableName: `${config.database}.${config.table}_candle_1h`,
    bucketMs: 3600_000,
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

  return queryClickHouse<RawCandleRow>(`
    SELECT
      bucket_time,
      multiIf(${whereA}, 'a', ${whereB}, 'b', '') AS side,
      open_mid,
      high_mid,
      low_mid,
      close_mid,
      final_bid_price,
      final_bid_size,
      final_bid_order_count,
      final_ask_price,
      final_ask_size,
      final_ask_order_count
    FROM ${tableName} AS c FINAL
    WHERE (${whereA} OR ${whereB})
      AND bucket_time >= fromUnixTimestamp64Milli(${numericLiteral(fromMs)})
      AND bucket_time < fromUnixTimestamp64Milli(${numericLiteral(toMs)})
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

  return queryClickHouse<RawCandleRow>(`
    SELECT
      bucket_time,
      multiIf(${whereA}, 'a', ${whereB}, 'b', '') AS side,
      open_mid,
      high_mid,
      low_mid,
      close_mid,
      final_bid_price,
      final_bid_size,
      final_bid_order_count,
      final_ask_price,
      final_ask_size,
      final_ask_order_count
    FROM ${candleConfig.tableName} AS c FINAL
    WHERE (${whereA} OR ${whereB})
      AND bucket_time < fromUnixTimestamp64Milli(${numericLiteral(fromMs)})
    ORDER BY bucket_time DESC
    LIMIT 1 BY venue_instance_id, instrument_id
    FORMAT JSONEachRow
  `);
}

// ── Candle rows → SpreadPoints ────────────────────────────────────────────

/**
 * Convert candle rows into SpreadPoint[] by pairing 'a' and 'b' records
 * within the same bucket, then carry-forward across empty buckets.
 *
 * Rows with bucket_time < fromMs are treated as seed state (carry-forward
 * baseline). Rows within [fromMs, toMs) produce spread points.
 */
export function convertCandleRowsToSpreadPoints(
  candleRows: RawCandleRow[],
  fromMs: number,
  toMs: number,
  bucketMs: number,
  _instrumentA: Instrument,
  _instrumentB: Instrument,
  aRate: number,
  bRate: number,
): SpreadPoint[] {
  // Sort all rows by bucket_time ASC, side ASC
  const sorted = [...candleRows].sort((left, right) => {
    const tsL = new Date(left.bucket_time).getTime();
    const tsR = new Date(right.bucket_time).getTime();
    if (tsL !== tsR) return tsL - tsR;
    return left.side.localeCompare(right.side);
  });

  // Seed lastA / lastB from rows before the window
  let lastA: RawCandleRow | undefined;
  let lastB: RawCandleRow | undefined;

  // Index rows within the window by bucket time for fast lookup
  const byBucket = new Map<number, { a?: RawCandleRow; b?: RawCandleRow }>();
  for (const row of sorted) {
    const ts = new Date(row.bucket_time).getTime();
    if (!Number.isFinite(ts)) continue;

    if (ts < fromMs) {
      // Seed: update carry-forward baseline
      if (row.side === 'a') lastA = row;
      else if (row.side === 'b') lastB = row;
    } else if (ts < toMs) {
      // In-window row
      if (!byBucket.has(ts)) byBucket.set(ts, {});
      const entry = byBucket.get(ts)!;
      if (row.side === 'a') entry.a = row;
      else if (row.side === 'b') entry.b = row;
    }
  }

  // Walk bucket boundaries, carry-forward last known state
  const points: SpreadPoint[] = [];

  for (let bucketStart = fromMs; bucketStart < toMs; bucketStart += bucketMs) {
    const entry = byBucket.get(bucketStart);
    if (entry?.a) lastA = entry.a;
    if (entry?.b) lastB = entry.b;

    if (lastA && lastB) {
      points.push(buildSpreadPoint(bucketStart + bucketMs, lastA, lastB, aRate, bRate));
    }
  }

  return points;
}

// ── Single spread point from paired candle states ─────────────────────────

function buildSpreadPoint(
  tsMs: number,
  a: RawCandleRow,
  b: RawCandleRow,
  aRate: number,
  bRate: number,
): SpreadPoint {
  const aBid = (a.final_bid_price ?? 0) * aRate;
  const aAsk = (a.final_ask_price ?? 0) * aRate;
  const bBid = (b.final_bid_price ?? 0) * bRate;
  const bAsk = (b.final_ask_price ?? 0) * bRate;
  const aMid = (a.close_mid ?? 0) * aRate;
  const bMid = (b.close_mid ?? 0) * bRate;
  const aToB = aBid - bAsk;
  const bToA = bBid - aAsk;

  return {
    tsMs,
    aBid,
    aAsk,
    aBidSize: a.final_bid_size,
    aAskSize: a.final_ask_size,
    aBidSizeText: null,
    aAskSizeText: null,
    aBidOrderCount: a.final_bid_order_count,
    aAskOrderCount: a.final_ask_order_count,
    bBid,
    bAsk,
    bBidSize: b.final_bid_size,
    bAskSize: b.final_ask_size,
    bBidSizeText: null,
    bAskSizeText: null,
    bBidOrderCount: b.final_bid_order_count,
    bAskOrderCount: b.final_ask_order_count,
    aMid,
    bMid,
    aToB,
    bToA,
    aToBBp: bAsk === 0 ? null : (aToB / bAsk) * 10000,
    bToABp: aAsk === 0 ? null : (bToA / aAsk) * 10000,
    midDiff: aMid - bMid,
  };
}
