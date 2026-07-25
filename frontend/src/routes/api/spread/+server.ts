import { json, type RequestHandler } from '@sveltejs/kit';
import type {
  QuoteRate,
  SpreadGranularity,
  SpreadPoint,
  SpreadRequestedPrecision,
  SpreadResponse,
  SpreadSource
} from '$lib/types';
import { validBookWhere } from '$lib/server/book-filter';
import {
  candleCoversRange,
  getCandleCapability
} from '$lib/server/candle-capability';
import { fetchInstrumentMetadata } from '$lib/server/catalog';
import {
  ClickHouseError,
  clickHouseConfig,
  numericLiteral,
  queryClickHouse,
  tickTable
} from '$lib/server/clickhouse';
import { resolveRates } from '$lib/server/rates';
import {
  buildBucketSnapshotSpreadPoints,
  buildEventSpreadPoints,
  type RawTickRow
} from '$lib/server/spread-state';
import { getTickSchema, tickIdentityWhere, type TickSchema } from '$lib/server/tick-schema';
import {
  candleGranularityForBucketSeconds,
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
const MAX_RESPONSE_POINTS = 5_000;
const MAX_RAW_TICK_ROWS = MAX_RESPONSE_POINTS;
const SPREAD_QUERY_OPTIONS = { maxThreads: 2 } as const;
const SPREAD_CACHE_TTL_MS = 30_000;
const MAX_SPREAD_CACHE_ENTRIES = 48;
const MAX_CONCURRENT_SPREAD_QUERIES = 6;
const MAX_QUEUED_SPREAD_QUERIES = 24;

type SpreadRequest = {
  catalogA?: unknown;
  catalogB?: unknown;
  fromMs?: unknown;
  toMs?: unknown;
  bucketSeconds?: unknown;
  precision?: unknown;
  afterCursor?: unknown;
  rates?: QuoteRate[];
};

type SpreadQueryResult = {
  points: SpreadPoint[];
  sourceRows: number;
  coverage?: { fromMs: number; toMs: number };
};

type BaseSpreadInput = {
  instrumentA: SpreadResponse['meta']['instrumentA'];
  instrumentB: SpreadResponse['meta']['instrumentB'];
  fromMs: number;
  toMs: number;
  aRate: number;
  bRate: number;
  maxStaleMs: number;
  tickSchema: Awaited<ReturnType<typeof getTickSchema>>;
};

type BookRowsInput = Omit<BaseSpreadInput, 'aRate' | 'bRate' | 'maxStaleMs'>;

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
const spreadQueryWaiters: Array<() => void> = [];
let activeSpreadQueries = 0;

export const POST: RequestHandler = async ({ request }) => {
  const requestId = crypto.randomUUID();
  try {
    const payload = (await request.json()) as SpreadRequest;
    const catalogA = validateCatalogId(payload.catalogA, 'catalogA');
    const catalogB = validateCatalogId(payload.catalogB, 'catalogB');
    if (catalogA === catalogB) {
      throw publicError('Choose two different instruments', 400, 'INVALID_PAIR');
    }

    const fromMs = parseTimestamp(payload.fromMs, 'fromMs');
    const toMs = parseTimestamp(payload.toMs, 'toMs');
    if (fromMs >= toMs) {
      throw publicError('fromMs must be before toMs', 400, 'INVALID_RANGE');
    }
    if (toMs - fromMs > MAX_RANGE_MS) {
      throw publicError('Time range is capped at 31 days', 422, 'RANGE_TOO_LARGE');
    }

    const [instrumentA, instrumentB] = await fetchSelectedInstruments(catalogA, catalogB);
    if (instrumentA.baseAsset !== instrumentB.baseAsset) {
      throw publicError(
        'Selected instruments must share the same base asset',
        400,
        'BASE_ASSET_MISMATCH'
      );
    }
    if (
      instrumentA.venueInstanceId === instrumentB.venueInstanceId &&
      instrumentA.instrumentId === instrumentB.instrumentId
    ) {
      throw publicError('Choose two distinct venue instruments', 400, 'INVALID_PAIR');
    }

    const { targetQuote, aRate, bRate } = resolveRates(
      instrumentA.quoteAsset,
      instrumentB.quoteAsset,
      payload.rates
    );
    const requestedPrecision = parseRequestedPrecision(payload.precision);
    const afterCursor = parseAfterCursor(payload.afterCursor);
    const execution = await resolveExecution(
      requestedPrecision,
      payload.bucketSeconds,
      fromMs,
      toMs
    );
    const { granularity, source, bucketSeconds, fallbackReason, candleConfig } = execution;
    const tickSchema = await getTickSchema();
    const { maxStaleMs } = clickHouseConfig();
    const queryInput: BaseSpreadInput = {
      instrumentA,
      instrumentB,
      fromMs,
      toMs,
      aRate,
      bRate,
      maxStaleMs,
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
      fallbackReason ?? '',
      aRate,
      bRate
    ].join('|');
    const result = await cachedSpreadResult(cacheKey, () => {
      if (source === 'raw') {
        return fetchRawSpreadPoints(queryInput);
      }
      if (source === 'bucket') {
        return fetchBucketedSpreadPoints({
          ...queryInput,
          bucketSeconds
        });
      }
      if (!candleConfig) {
        throw new ClickHouseError('Candle execution is missing its granularity config');
      }
      return fetchCandleSpreadPoints({ ...queryInput, candleConfig });
    });

    const points = applyAfterCursor(result.points, afterCursor);
    if (points.length > MAX_RESPONSE_POINTS) {
      throw publicError(
        `The query would return more than ${MAX_RESPONSE_POINTS} points. Choose a coarser interval.`,
        422,
        'POINT_LIMIT_EXCEEDED'
      );
    }
    const resultCoverage = result.coverage ?? { fromMs, toMs };

    const response: SpreadResponse = {
      meta: {
        fromMs,
        toMs,
        bucketSeconds,
        granularity,
        requestedPrecision,
        source,
        fallbackReason,
        coverage: {
          fromMs: resultCoverage.fromMs,
          toMs: resultCoverage.toMs,
          complete: resultCoverage.fromMs <= fromMs && resultCoverage.toMs >= toMs
        },
        nextCursor: points.at(-1)?.id ?? afterCursor,
        sourceRows: result.sourceRows,
        bookStatePolicy: 'carry_forward_with_expiry',
        maxStaleMs,
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
        'cache-control': 'no-store',
        'x-request-id': requestId
      }
    });
  } catch (error) {
    return apiError(error, requestId);
  }
};

type SpreadExecution = {
  source: SpreadSource;
  granularity: SpreadGranularity;
  bucketSeconds: number;
  fallbackReason: SpreadResponse['meta']['fallbackReason'];
  candleConfig: CandleGranularityConfig | null;
};

async function resolveExecution(
  requested: SpreadRequestedPrecision,
  rawBucketSeconds: unknown,
  fromMs: number,
  toMs: number
): Promise<SpreadExecution> {
  const rangeMs = toMs - fromMs;
  if (requested === 'raw') {
    if (rangeMs > RAW_EXPLICIT_MAX_RANGE_MS) {
      throw publicError(
        'Raw tick mode is capped at 6 hours. Use a shorter range or a bucket interval.',
        422,
        'RAW_RANGE_TOO_LARGE'
      );
    }
    return {
      source: 'raw',
      granularity: 'raw',
      bucketSeconds: 0,
      fallbackReason: null,
      candleConfig: null
    };
  }

  if (requested === 'auto' && rangeMs <= RAW_AUTO_MAX_RANGE_MS) {
    const bucketSeconds = parseBucketSeconds(undefined, fromMs, toMs, false);
    return {
      source: 'bucket',
      granularity: 'bucket',
      bucketSeconds,
      fallbackReason: null,
      candleConfig: null
    };
  }

  if (requested === 'bucket') {
    const bucketSeconds = parseBucketSeconds(rawBucketSeconds, fromMs, toMs, true);
    return {
      source: 'bucket',
      granularity: 'bucket',
      bucketSeconds,
      fallbackReason: null,
      candleConfig: null
    };
  }

  const candleConfig = resolveCandleGranularity(rawBucketSeconds, rangeMs);
  const capability = await getCandleCapability();
  const covered = candleCoversRange(capability, fromMs, toMs);
  if (capability.ready && covered) {
    return {
      source: 'candle',
      granularity: candleConfig.granularity,
      bucketSeconds: candleConfig.bucketMs / 1000,
      fallbackReason: null,
      candleConfig
    };
  }
  if (capability.mode === 'force') {
    throw publicError(
      covered
        ? 'Candle schema v2 is not ready.'
        : 'The requested range is outside validated candle coverage.',
      503,
      covered ? 'CANDLE_NOT_READY' : 'CANDLE_COVERAGE_GAP'
    );
  }

  const bucketSeconds = parseBucketSeconds(undefined, fromMs, toMs, false);
  return {
    source: 'bucket',
    granularity: 'bucket',
    bucketSeconds,
    fallbackReason:
      capability.reason === 'candle_disabled'
        ? 'candle_disabled'
        : capability.ready
          ? 'coverage_gap'
          : 'candle_not_ready',
    candleConfig: null
  };
}

function resolveCandleGranularity(
  rawBucketSeconds: unknown,
  rangeMs: number
): CandleGranularityConfig {
  if (rawBucketSeconds !== undefined && rawBucketSeconds !== null) {
    if (
      typeof rawBucketSeconds !== 'number' ||
      !Number.isFinite(rawBucketSeconds) ||
      !Number.isInteger(rawBucketSeconds) ||
      rawBucketSeconds <= 0
    ) {
      throw publicError(
        'Candle bucketSeconds must be 1, 60, 300, 900, or 3600.',
        400,
        'INVALID_PRECISION'
      );
    }
    const requested = candleGranularityForBucketSeconds(rawBucketSeconds);
    if (!requested) {
      throw publicError(
        'Candle bucketSeconds must be 1, 60, 300, 900, or 3600.',
        400,
        'INVALID_PRECISION'
      );
    }
    return requested;
  }
  const selected = selectGranularity(rangeMs);
  return selected.granularity === 'raw'
    ? candleGranularityForBucketSeconds(1)!
    : selected;
}

async function fetchSelectedInstruments(catalogA: string, catalogB: string) {
  const instruments = await fetchInstrumentMetadata([catalogA, catalogB]);
  const instrumentA = instruments.find((instrument) => instrument.catalogId === catalogA);
  const instrumentB = instruments.find((instrument) => instrument.catalogId === catalogB);
  if (!instrumentA || !instrumentB) {
    throw publicError(
      'Selected instrument was not found in instrument_catalog',
      400,
      'INSTRUMENT_NOT_FOUND'
    );
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

  const promise = withSpreadQuerySlot(load).catch((error) => {
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

async function withSpreadQuerySlot<T>(load: () => Promise<T>): Promise<T> {
  if (activeSpreadQueries >= MAX_CONCURRENT_SPREAD_QUERIES) {
    if (spreadQueryWaiters.length >= MAX_QUEUED_SPREAD_QUERIES) {
      throw publicError(
        'The spread service is busy. Retry shortly.',
        503,
        'QUERY_CAPACITY_EXCEEDED'
      );
    }
    await new Promise<void>((resolve) => spreadQueryWaiters.push(resolve));
  }
  activeSpreadQueries += 1;
  try {
    return await load();
  } finally {
    activeSpreadQueries -= 1;
    spreadQueryWaiters.shift()?.();
  }
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
    points: buildEventSpreadPoints(
      seedRows,
      tickRows,
      input.aRate,
      input.bRate,
      input.maxStaleMs
    ),
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
      input.bRate,
      input.maxStaleMs
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

  const alignedFromMs = Math.ceil(fromMs / candleConfig.bucketMs) * candleConfig.bucketMs;
  const alignedToMs = Math.floor(toMs / candleConfig.bucketMs) * candleConfig.bucketMs;
  return {
    points: convertCandleRowsToSpreadPoints(
      allRows,
      fromMs,
      toMs,
      candleConfig.bucketMs,
      instrumentA,
      instrumentB,
      aRate,
      bRate,
      input.maxStaleMs
    ),
    sourceRows: tickRows.length,
    coverage: {
      fromMs: alignedFromMs,
      toMs: alignedToMs
    }
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
    throw publicError(
      `Spread query is capped at ${input.maxRows} source rows. Reduce the time range or use a coarser window.`,
      422,
      'POINT_LIMIT_EXCEEDED'
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
    throw publicError(`${label} is required`, 400, 'INVALID_REQUEST');
  }
  return value.trim();
}

function parseTimestamp(value: unknown, label: string): number {
  const parsed =
    typeof value === 'number' ? value : typeof value === 'string' ? Date.parse(value) : Number.NaN;
  if (!Number.isFinite(parsed)) {
    throw publicError(`${label} must be a timestamp`, 400, 'INVALID_REQUEST');
  }
  return Math.trunc(parsed);
}

function parseRequestedPrecision(value: unknown): SpreadRequestedPrecision {
  if (value === undefined || value === null || value === '') return 'auto';
  if (value === 'raw' || value === 'bucket' || value === 'candle') return value;
  throw publicError('precision must be raw, bucket, or candle', 400, 'INVALID_PRECISION');
}

function parseAfterCursor(value: unknown): string | null {
  if (value === undefined || value === null || value === '') return null;
  if (typeof value !== 'string' || value.length > 160) {
    throw publicError('afterCursor must be a short string', 400, 'INVALID_CURSOR');
  }
  return value;
}

function parseBucketSeconds(
  value: unknown,
  fromMs: number,
  toMs: number,
  explicit: boolean
): number {
  let bucketSeconds: number;
  if (typeof value === 'number' && Number.isFinite(value) && value > 0) {
    bucketSeconds = clamp(Math.trunc(value), 1, 3600);
  } else {
    const rangeSeconds = Math.max(1, (toMs - fromMs) / 1000);
    bucketSeconds = clamp(Math.ceil(rangeSeconds / TARGET_POINTS), 1, 3600);
  }
  const pointCount = Math.ceil((toMs - fromMs) / (bucketSeconds * 1000));
  if (pointCount > MAX_RESPONSE_POINTS) {
    const suggested = Math.ceil((toMs - fromMs) / 1000 / MAX_RESPONSE_POINTS);
    throw publicError(
      `${explicit ? 'The selected interval' : 'The query'} would return too many points. Use at least ${suggested} seconds.`,
      422,
      'POINT_LIMIT_EXCEEDED'
    );
  }
  return bucketSeconds;
}

function sampleSecondsForBucket(bucketSeconds: number): number {
  return clamp(Math.trunc(bucketSeconds), 1, 3600);
}

function clamp(value: number, min: number, max: number): number {
  return Math.min(max, Math.max(min, value));
}

function applyAfterCursor(points: SpreadPoint[], cursor: string | null): SpreadPoint[] {
  if (!cursor) return points;
  const index = points.findIndex((point) => point.id === cursor);
  return index < 0 ? points : points.slice(index + 1);
}

function publicError(message: string, status: number, code: string): ClickHouseError {
  return new ClickHouseError(message, status, true, code);
}

function apiError(error: unknown, requestId: string): Response {
  console.error(`[api/spread:${requestId}]`, error);
  const exposed = error instanceof ClickHouseError && error.expose;
  const status = exposed ? error.status : 502;
  const message = exposed ? error.message : 'The market data service is temporarily unavailable.';
  const code = exposed ? error.code : 'UPSTREAM_UNAVAILABLE';
  return json(
    { error: message, code, requestId },
    { status, headers: { 'cache-control': 'no-store', 'x-request-id': requestId } }
  );
}
