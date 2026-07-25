import { json, type RequestHandler } from '@sveltejs/kit';
import type { QuoteRate, SpreadPoint, SpreadResponse } from '$lib/types';
import { validBookWhere } from '$lib/server/book-filter';
import { fetchInstrumentMetadata } from '$lib/server/catalog';
import { ClickHouseError, numericLiteral, queryClickHouse, tickTable } from '$lib/server/clickhouse';
import { resolveRates } from '$lib/server/rates';
import {
  buildBucketSnapshotSpreadPoints,
  buildEventSpreadPoints,
  type RawTickRow
} from '$lib/server/spread-state';
import { getTickSchema, tickIdentityWhere, type TickSchema } from '$lib/server/tick-schema';
import {
  convertCandleRowsToSpreadPoints,
  fetchCandleRows,
  fetchCandleSeedRows,
  selectGranularity,
  type CandleGranularityConfig,
  type RawCandleRow
} from '$lib/server/candles';

const MAX_RANGE_MS = 31 * 24 * 60 * 60 * 1000;
const TARGET_POINTS = 420;
const RAW_EXPLICIT_MAX_RANGE_MS = 6 * 60 * 60 * 1000;
const RAW_AUTO_MAX_RANGE_MS = 60_000; // 1 minute
const MAX_RAW_TICK_ROWS = 100_000;
const SPREAD_QUERY_OPTIONS = { maxThreads: 2 } as const;
const SPREAD_CACHE_TTL_MS = 30_000;
const MAX_SPREAD_CACHE_ENTRIES = 48;

type SpreadRequest = {
  catalogA?: unknown;
  catalogB?: unknown;
  fromMs?: unknown;
  toMs?: unknown;
  bucketSeconds?: unknown;
  precision?: unknown;
  rates?: QuoteRate[];
};

type SpreadGranularity = SpreadResponse['meta']['granularity'];

type SpreadQueryResult = {
  points: SpreadPoint[];
  sourceRows: number;
};

type BaseSpreadInput = {
  instrumentA: SpreadResponse['meta']['instrumentA'];
  instrumentB: SpreadResponse['meta']['instrumentB'];
  fromMs: number;
  toMs: number;
  aRate: number;
  bRate: number;
  tickSchema: Awaited<ReturnType<typeof getTickSchema>>;
};

type BookRowsInput = Omit<BaseSpreadInput, 'aRate' | 'bRate'>;

type ExactBookRowsInput = BookRowsInput & {
  maxRows: number;
};

type SampledBookRowsInput = BookRowsInput & {
  sampleSeconds: number;
};

type BookRowsResult = {
  seedRows: RawTickRow[];
  tickRows: RawTickRow[];
};

type CandleBookInput = BaseSpreadInput & {
  candleConfig: CandleGranularityConfig;
};

type CachedSpreadResult = {
  expiresAt: number;
  promise: Promise<SpreadQueryResult>;
};

const spreadResultCache = new Map<string, CachedSpreadResult>();

export const POST: RequestHandler = async ({ request }) => {
  try {
    const payload = (await request.json()) as SpreadRequest;
    const catalogA = validateCatalogId(payload.catalogA, 'catalogA');
    const catalogB = validateCatalogId(payload.catalogB, 'catalogB');
    if (catalogA === catalogB) {
      throw new ClickHouseError('Choose two different instruments', 400);
    }

    const fromMs = parseTimestamp(payload.fromMs, 'fromMs');
    const toMs = parseTimestamp(payload.toMs, 'toMs');
    if (fromMs >= toMs) {
      throw new ClickHouseError('fromMs must be before toMs', 400);
    }
    if (toMs - fromMs > MAX_RANGE_MS) {
      throw new ClickHouseError('Time range is capped at 31 days', 400);
    }

    const [instrumentA, instrumentB] = await fetchSelectedInstruments(catalogA, catalogB);
    if (instrumentA.baseAsset !== instrumentB.baseAsset) {
      throw new ClickHouseError('Selected instruments must share the same base asset', 400);
    }
    if (
      instrumentA.venueInstanceId === instrumentB.venueInstanceId &&
      instrumentA.instrumentId === instrumentB.instrumentId
    ) {
      throw new ClickHouseError('Choose two distinct venue instruments', 400);
    }

    const { targetQuote, aRate, bRate } = resolveRates(
      instrumentA.quoteAsset,
      instrumentB.quoteAsset,
      payload.rates
    );
    const rangeMs = toMs - fromMs;
    const granularity = resolveGranularity(payload.precision, rangeMs);
    const bucketSeconds =
      granularity === 'bucket' ? parseBucketSeconds(payload.bucketSeconds, fromMs, toMs) : 0;
    const tickSchema = await getTickSchema();
    const queryInput: BaseSpreadInput = {
      instrumentA,
      instrumentB,
      fromMs,
      toMs,
      aRate,
      bRate,
      tickSchema
    };
    const cacheKey = [
      instrumentA.venueInstanceId,
      instrumentA.instrumentId,
      instrumentB.venueInstanceId,
      instrumentB.instrumentId,
      fromMs,
      toMs,
      granularity,
      bucketSeconds,
      aRate,
      bRate
    ].join('|');
    const result = await cachedSpreadResult(cacheKey, () => {
      if (granularity === 'raw') {
        return fetchRawSpreadPoints(queryInput);
      }
      if (granularity === 'bucket') {
        return fetchBucketedSpreadPoints({
          ...queryInput,
          bucketSeconds
        });
      }
      // Candle granularity: 1s, 1m, 5m, 15m, 1h
      const candleConfig = selectGranularity(rangeMs);
      return fetchCandleSpreadPoints({ ...queryInput, candleConfig });
    });

    const points = result.points;

    const response: SpreadResponse = {
      meta: {
        fromMs,
        toMs,
        bucketSeconds,
        granularity,
        sourceRows: result.sourceRows,
        bookStatePolicy: 'carry_forward',
        maxStaleMs: null,
        targetQuote,
        aRate,
        bRate,
        instrumentA,
        instrumentB
      },
      points
    };
    return json(response, {
      headers: {
        'cache-control': 'private, max-age=15'
      }
    });
  } catch (error) {
    return apiError(error);
  }
};

async function fetchSelectedInstruments(catalogA: string, catalogB: string) {
  const instruments = await fetchInstrumentMetadata([catalogA, catalogB]);
  const instrumentA = instruments.find((instrument) => instrument.catalogId === catalogA);
  const instrumentB = instruments.find((instrument) => instrument.catalogId === catalogB);
  if (!instrumentA || !instrumentB) {
    throw new ClickHouseError('Selected instrument was not found in instrument_catalog', 400);
  }
  return [instrumentA, instrumentB] as const;
}

function cachedSpreadResult(
  key: string,
  load: () => Promise<SpreadQueryResult>
): Promise<SpreadQueryResult> {
  const now = Date.now();
  const cached = spreadResultCache.get(key);
  if (cached && cached.expiresAt > now) {
    spreadResultCache.delete(key);
    spreadResultCache.set(key, cached);
    return cached.promise;
  }
  if (cached) spreadResultCache.delete(key);

  const promise = load().catch((error) => {
    spreadResultCache.delete(key);
    throw error;
  });
  spreadResultCache.set(key, {
    expiresAt: now + SPREAD_CACHE_TTL_MS,
    promise
  });
  while (spreadResultCache.size > MAX_SPREAD_CACHE_ENTRIES) {
    const oldest = spreadResultCache.keys().next().value;
    if (typeof oldest !== 'string') break;
    spreadResultCache.delete(oldest);
  }
  return promise;
}

// ── Raw tick path (≤ 1 minute) ────────────────────────────────────────────

async function fetchRawSpreadPoints(input: BaseSpreadInput): Promise<SpreadQueryResult> {
  const { seedRows, tickRows } = await fetchExactBookRows({
    instrumentA: input.instrumentA,
    instrumentB: input.instrumentB,
    fromMs: input.fromMs,
    toMs: input.toMs,
    tickSchema: input.tickSchema,
    maxRows: MAX_RAW_TICK_ROWS
  });

  return {
    points: buildEventSpreadPoints(seedRows, tickRows, input.aRate, input.bRate),
    sourceRows: tickRows.length
  };
}

// ── Bucketed tick path (backward compatible, ≤ 6h explicit) ───────────────

async function fetchBucketedSpreadPoints(
  input: BaseSpreadInput & { bucketSeconds: number }
): Promise<SpreadQueryResult> {
  const bookInput = {
    instrumentA: input.instrumentA,
    instrumentB: input.instrumentB,
    fromMs: input.fromMs,
    toMs: input.toMs,
    tickSchema: input.tickSchema
  };
  const { seedRows, tickRows } = await fetchSampledBookRows({
    ...bookInput,
    sampleSeconds: sampleSecondsForBucket(input.bucketSeconds)
  });

  return {
    points: buildBucketSnapshotSpreadPoints(
      seedRows,
      tickRows,
      input.fromMs,
      input.toMs,
      input.bucketSeconds,
      input.aRate,
      input.bRate
    ),
    sourceRows: tickRows.length
  };
}

// ── Candle path (auto-selected for ranges > 1 minute) ────────────────────

async function fetchCandleSpreadPoints(input: CandleBookInput): Promise<SpreadQueryResult> {
  const { tickSchema, instrumentA, instrumentB, fromMs, toMs, aRate, bRate, candleConfig } = input;

  const [seedRows, tickRows] = await Promise.all([
    fetchCandleSeedRows(tickSchema, instrumentA, instrumentB, fromMs, candleConfig),
    fetchCandleRows(tickSchema, instrumentA, instrumentB, fromMs, toMs, candleConfig)
  ]);

  const allRows = [...seedRows, ...tickRows];

  return {
    points: convertCandleRowsToSpreadPoints(
      allRows,
      fromMs,
      toMs,
      candleConfig.bucketMs,
      instrumentA,
      instrumentB,
      aRate,
      bRate
    ),
    sourceRows: tickRows.length
  };
}

// ── Raw tick query helpers ────────────────────────────────────────────────

async function fetchExactBookRows(input: ExactBookRowsInput): Promise<BookRowsResult> {
  const whereA = tickIdentityWhere(input.tickSchema, input.instrumentA, 'ticks');
  const whereB = tickIdentityWhere(input.tickSchema, input.instrumentB, 'ticks');
  const validBooks = validBookWhere(input.tickSchema, 'ticks');
  const limit = input.maxRows + 1;

  const [seedRows, tickRows] = await Promise.all([
    fetchSeedRows(input),
    queryClickHouse<RawTickRow>(`
WITH
  fromUnixTimestamp64Milli(${numericLiteral(input.fromMs)}) AS start_time,
  fromUnixTimestamp64Milli(${numericLiteral(input.toMs)}) AS end_time
SELECT
  if(${whereA}, 'a', 'b') AS side,
  toUnixTimestamp64Milli(ticks.recv_time) AS tsMs,
  ticks.recv_ts_ns AS tsNs,
  ticks.bid_price AS bid,
  ticks.ask_price AS ask,
  ticks.bid_size AS bidSize,
  ticks.ask_size AS askSize,
  ticks.bid_size_text AS bidSizeText,
  ticks.ask_size_text AS askSizeText,
  ticks.bid_order_count AS bidOrderCount,
  ticks.ask_order_count AS askOrderCount,
  ticks.mid AS mid
FROM ${tickTable()} AS ticks
WHERE ((${whereA}) OR (${whereB}))
  AND ticks.recv_time >= start_time
  AND ticks.recv_time <= end_time
  AND ${validBooks}
ORDER BY tsMs ASC, tsNs ASC, side ASC
LIMIT ${limit}
FORMAT JSONEachRow
`, SPREAD_QUERY_OPTIONS)
  ]);

  if (tickRows.length > input.maxRows) {
    throw new ClickHouseError(
      `Spread query is capped at ${input.maxRows} source rows. Reduce the time range or use a coarser window.`,
      400
    );
  }

  return { seedRows, tickRows };
}

async function fetchSampledBookRows(input: SampledBookRowsInput): Promise<BookRowsResult> {
  const whereA = tickIdentityWhere(input.tickSchema, input.instrumentA, 'ticks');
  const whereB = tickIdentityWhere(input.tickSchema, input.instrumentB, 'ticks');
  const validBooks = validBookWhere(input.tickSchema, 'ticks');
  const sampleSeconds = Math.max(1, Math.trunc(input.sampleSeconds));

  const [seedRows, tickRows] = await Promise.all([
    fetchSeedRows(input),
    queryClickHouse<RawTickRow>(`
WITH
  fromUnixTimestamp64Milli(${numericLiteral(input.fromMs)}) AS start_time,
  fromUnixTimestamp64Milli(${numericLiteral(input.toMs)}) AS end_time
SELECT *
FROM
(
  ${sampledCandidateSelect(whereA, whereB, validBooks, sampleSeconds, input.fromMs)}
)
ORDER BY tsMs ASC, tsNs ASC, side ASC
FORMAT JSONEachRow
`, SPREAD_QUERY_OPTIONS)
  ]);

  return { seedRows, tickRows };
}

async function fetchSeedRows(input: BookRowsInput): Promise<RawTickRow[]> {
  const alias = 'seed_ticks';
  const whereA = tickIdentityWhere(input.tickSchema, input.instrumentA, alias);
  const whereB = tickIdentityWhere(input.tickSchema, input.instrumentB, alias);
  const validBooks = validBookWhere(input.tickSchema, alias);
  const rowTuple = tickRowTuple(alias);
  // Bound the scan to the table retention horizon. This is a query optimization,
  // not a freshness timeout: once found, the book remains valid throughout the window.
  const seedFromMs = input.fromMs - MAX_RANGE_MS;

  return queryClickHouse<RawTickRow>(`
WITH
  fromUnixTimestamp64Milli(${numericLiteral(seedFromMs)}) AS seed_start_time,
  fromUnixTimestamp64Milli(${numericLiteral(input.fromMs)}) AS start_time
SELECT
  side,
  toUnixTimestamp64Milli(tupleElement(latest_row, 1)) AS tsMs,
  tupleElement(latest_row, 2) AS tsNs,
  tupleElement(latest_row, 3) AS bid,
  tupleElement(latest_row, 4) AS ask,
  tupleElement(latest_row, 5) AS bidSize,
  tupleElement(latest_row, 6) AS askSize,
  tupleElement(latest_row, 7) AS bidSizeText,
  tupleElement(latest_row, 8) AS askSizeText,
  tupleElement(latest_row, 9) AS bidOrderCount,
  tupleElement(latest_row, 10) AS askOrderCount,
  tupleElement(latest_row, 11) AS mid
FROM
(
  SELECT
    if(${whereA}, 'a', 'b') AS side,
    argMax(${rowTuple}, ${alias}.recv_ts_ns) AS latest_row
  FROM ${tickTable()} AS ${alias}
  WHERE ((${whereA}) OR (${whereB}))
    AND ${alias}.recv_time >= seed_start_time
    AND ${alias}.recv_time < start_time
    AND ${validBooks}
  GROUP BY side
)
FORMAT JSONEachRow
`, SPREAD_QUERY_OPTIONS);
}

function sampledCandidateSelect(
  whereA: string,
  whereB: string,
  validBooks: string,
  sampleSeconds: number,
  fromMs: number
) {
  const alias = 'ticks';
  const rowTuple = tickRowTuple(alias);
  const sampleMs = sampleSeconds * 1000;
  return `
  SELECT
    side,
    toUnixTimestamp64Milli(tupleElement(latest_row, 1)) AS tsMs,
    tupleElement(latest_row, 2) AS tsNs,
    tupleElement(latest_row, 3) AS bid,
    tupleElement(latest_row, 4) AS ask,
    tupleElement(latest_row, 5) AS bidSize,
    tupleElement(latest_row, 6) AS askSize,
    tupleElement(latest_row, 7) AS bidSizeText,
    tupleElement(latest_row, 8) AS askSizeText,
    tupleElement(latest_row, 9) AS bidOrderCount,
    tupleElement(latest_row, 10) AS askOrderCount,
    tupleElement(latest_row, 11) AS mid
  FROM
  (
    SELECT
      if(${whereA}, 'a', 'b') AS side,
      argMax(${rowTuple}, ${alias}.recv_ts_ns) AS latest_row
    FROM ${tickTable()} AS ${alias}
    WHERE ((${whereA}) OR (${whereB}))
      AND ${alias}.recv_time >= start_time
      AND ${alias}.recv_time <= end_time
      AND ${validBooks}
    GROUP BY
      side,
      intDiv(
        toUnixTimestamp64Milli(${alias}.recv_time) - ${numericLiteral(fromMs)},
        ${numericLiteral(sampleMs)}
      )
  )
`;
}

function tickRowTuple(alias: string): string {
  return `tuple(
      ${alias}.recv_time,
      ${alias}.recv_ts_ns,
      ${alias}.bid_price,
      ${alias}.ask_price,
      ${alias}.bid_size,
      ${alias}.ask_size,
      ${alias}.bid_size_text,
      ${alias}.ask_size_text,
      ${alias}.bid_order_count,
      ${alias}.ask_order_count,
      ${alias}.mid
    )`;
}

// ── Validation helpers ────────────────────────────────────────────────────

function validateCatalogId(value: unknown, label: string): string {
  if (typeof value !== 'string' || value.trim().length === 0 || value.length > 256) {
    throw new ClickHouseError(`${label} is required`, 400);
  }
  return value.trim();
}

function parseTimestamp(value: unknown, label: string): number {
  const parsed =
    typeof value === 'number' ? value : typeof value === 'string' ? Date.parse(value) : Number.NaN;
  if (!Number.isFinite(parsed)) {
    throw new ClickHouseError(`${label} must be a timestamp`, 400);
  }
  return Math.trunc(parsed);
}

function resolveGranularity(value: unknown, rangeMs: number): SpreadGranularity {
  if (value === 'raw') {
    if (rangeMs > RAW_EXPLICIT_MAX_RANGE_MS) {
      throw new ClickHouseError(
        'Raw tick mode is capped at 6 hours. Use a shorter range or bucketed mode.',
        400
      );
    }
    return 'raw';
  }
  if (value === 'bucket') return 'bucket';
  if (value === 'candle') {
    // Explicit candle mode: use auto-selected granularity
    return selectGranularity(rangeMs).granularity;
  }

  // Auto-select: raw for ≤ 1 min, candle otherwise
  if (rangeMs <= RAW_AUTO_MAX_RANGE_MS) return 'raw';
  return selectGranularity(rangeMs).granularity;
}

function parseBucketSeconds(value: unknown, fromMs: number, toMs: number): number {
  if (typeof value === 'number' && Number.isFinite(value) && value > 0) {
    return clamp(Math.trunc(value), 1, 3600);
  }
  const rangeSeconds = Math.max(1, (toMs - fromMs) / 1000);
  return clamp(Math.ceil(rangeSeconds / TARGET_POINTS), 1, 3600);
}

function sampleSecondsForBucket(bucketSeconds: number): number {
  return clamp(Math.trunc(bucketSeconds), 1, 3600);
}

function clamp(value: number, min: number, max: number): number {
  return Math.min(max, Math.max(min, value));
}

function apiError(error: unknown): Response {
  const status = error instanceof ClickHouseError ? error.status : 500;
  const message = error instanceof Error ? error.message : 'Unknown server error';
  return json({ error: message }, { status });
}
