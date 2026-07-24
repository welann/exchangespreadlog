import { json, type RequestHandler } from '@sveltejs/kit';
import type { QuoteRate, SpreadPoint, SpreadResponse } from '$lib/types';
import { fetchInstrumentMetadata } from '$lib/server/catalog';
import { ClickHouseError, numericLiteral, queryClickHouse, tickTable } from '$lib/server/clickhouse';
import { resolveRates } from '$lib/server/rates';
import { getTickSchema, tickIdentityWhere, type TickSchema } from '$lib/server/tick-schema';

const MAX_RANGE_MS = 31 * 24 * 60 * 60 * 1000;
const TARGET_POINTS = 420;
const RAW_EXPLICIT_MAX_RANGE_MS = 6 * 60 * 60 * 1000;
const MAX_RAW_TICK_ROWS = 100_000;
const SPREAD_QUERY_OPTIONS = { maxThreads: 2 } as const;
const SPREAD_CACHE_TTL_MS = 20_000;
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
  maxStaleMs: number;
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

type CachedSpreadResult = {
  expiresAt: number;
  promise: Promise<SpreadQueryResult>;
};

type RawTickRow = {
  side: 'a' | 'b';
  tsMs: number | string;
  tsNs: number | string | null;
  bid: number | string | null;
  ask: number | string | null;
  bidSize: number | string | null;
  askSize: number | string | null;
  bidSizeText: string | null;
  askSizeText: string | null;
  bidOrderCount: number | string | null;
  askOrderCount: number | string | null;
  mid: number | string | null;
};

type TickSnapshot = {
  tsMs: number;
  bid: number;
  ask: number;
  bidSize: number | null;
  askSize: number | null;
  bidSizeText: string | null;
  askSizeText: string | null;
  bidOrderCount: number | null;
  askOrderCount: number | null;
  mid: number | null;
};

type TickEvent = {
  side: 'a' | 'b';
  snapshot: TickSnapshot;
  tsNs: bigint;
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
    const granularity = resolveGranularity(payload.precision, fromMs, toMs);
    const bucketSeconds =
      granularity === 'bucket' ? parseBucketSeconds(payload.bucketSeconds, fromMs, toMs) : 0;
    const maxStaleMs = maxStaleMsForBucket(granularity === 'bucket' ? bucketSeconds : 15);
    const tickSchema = await getTickSchema();
    const queryInput = {
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
      aRate,
      bRate,
      maxStaleMs
    ].join('|');
    const result = await cachedSpreadResult(cacheKey, () =>
      granularity === 'raw'
        ? fetchRawSpreadPoints(queryInput)
        : fetchBucketedSpreadPoints({
            ...queryInput,
            bucketSeconds
          })
    );

    const points = result.points;

    const response: SpreadResponse = {
      meta: {
        fromMs,
        toMs,
        bucketSeconds,
        granularity,
        sourceRows: result.sourceRows,
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

async function fetchRawSpreadPoints(input: BaseSpreadInput): Promise<SpreadQueryResult> {
  const { seedRows, tickRows } = await fetchExactBookRows({
    instrumentA: input.instrumentA,
    instrumentB: input.instrumentB,
    fromMs: input.fromMs,
    toMs: input.toMs,
    maxStaleMs: input.maxStaleMs,
    tickSchema: input.tickSchema,
    maxRows: MAX_RAW_TICK_ROWS
  });

  return {
    points: buildEventSpreadPoints(seedRows, tickRows, input.aRate, input.bRate, input.maxStaleMs),
    sourceRows: tickRows.length
  };
}

async function fetchBucketedSpreadPoints(
  input: BaseSpreadInput & { bucketSeconds: number }
): Promise<SpreadQueryResult> {
  const bookInput = {
    instrumentA: input.instrumentA,
    instrumentB: input.instrumentB,
    fromMs: input.fromMs,
    toMs: input.toMs,
    maxStaleMs: input.maxStaleMs,
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
  const seedFromMs = input.fromMs - input.maxStaleMs;

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

function validBookWhere(schema: TickSchema, alias: string): string {
  const prefix = `${alias}.`;
  const predicates = [
    `${prefix}bid_price IS NOT NULL`,
    `${prefix}ask_price IS NOT NULL`,
    `${prefix}bid_price > 0`,
    `${prefix}ask_price > 0`,
    `${prefix}bid_price <= ${prefix}ask_price`,
    `(${prefix}bid_size IS NULL OR ${prefix}bid_size > 0)`,
    `(${prefix}ask_size IS NULL OR ${prefix}ask_size > 0)`
  ];
  if (schema.hasQualityFlags) {
    predicates.push(
      `${prefix}quality_gap = false`,
      `${prefix}quality_stale = false`,
      `${prefix}quality_inconsistent = false`
    );
  }
  return predicates.join('\n      AND ');
}

function buildEventSpreadPoints(
  seedRows: RawTickRow[],
  tickRows: RawTickRow[],
  aRate: number,
  bRate: number,
  maxStaleMs: number
): SpreadPoint[] {
  const events = normalizeTickEvents(tickRows);
  const state = initialBookState(seedRows);
  const points: SpreadPoint[] = [];

  for (const event of events) {
    applyTickEvent(state, event);
    const point = pointFromFreshState(event.snapshot.tsMs, state.latestA, state.latestB, aRate, bRate, maxStaleMs);
    if (point) points.push(point);
  }

  return points;
}

function buildBucketSnapshotSpreadPoints(
  seedRows: RawTickRow[],
  tickRows: RawTickRow[],
  fromMs: number,
  toMs: number,
  bucketSeconds: number,
  aRate: number,
  bRate: number,
  maxStaleMs: number
): SpreadPoint[] {
  const bucketMs = Math.max(1, Math.trunc(bucketSeconds)) * 1000;
  const events = normalizeTickEvents(tickRows);
  const state = initialBookState(seedRows);
  const points: SpreadPoint[] = [];
  let eventIndex = 0;

  for (let bucketStart = fromMs; bucketStart < toMs; bucketStart += bucketMs) {
    const bucketEnd = Math.min(toMs, bucketStart + bucketMs);

    while (eventIndex < events.length) {
      const event = events[eventIndex];
      if (event.snapshot.tsMs > bucketEnd) break;

      applyTickEvent(state, event);
      eventIndex += 1;
    }

    const bucketPoint = pointFromFreshState(
      bucketEnd,
      state.latestA,
      state.latestB,
      aRate,
      bRate,
      maxStaleMs
    );
    if (bucketPoint) points.push(bucketPoint);
  }

  return points;
}

function initialBookState(seedRows: RawTickRow[]) {
  return {
    latestA: normalizeTick(seedRows.find((row) => row.side === 'a') ?? null),
    latestB: normalizeTick(seedRows.find((row) => row.side === 'b') ?? null)
  };
}

function normalizeTickEvents(rows: RawTickRow[]): TickEvent[] {
  return rows
    .map((row) => {
      const snapshot = normalizeTick(row);
      if (!snapshot) return null;
      return {
        side: row.side,
        snapshot,
        tsNs: nullableBigInt(row.tsNs) ?? BigInt(Math.trunc(snapshot.tsMs)) * 1_000_000n
      };
    })
    .filter((event): event is TickEvent => event !== null)
    .sort(compareTickEvents);
}

function compareTickEvents(left: TickEvent, right: TickEvent) {
  const tsDiff = left.snapshot.tsMs - right.snapshot.tsMs;
  if (tsDiff !== 0) return tsDiff;
  if (left.tsNs < right.tsNs) return -1;
  if (left.tsNs > right.tsNs) return 1;
  return left.side.localeCompare(right.side);
}

function applyTickEvent(
  state: { latestA: TickSnapshot | null; latestB: TickSnapshot | null },
  event: TickEvent
) {
  if (event.side === 'a') {
    state.latestA = event.snapshot;
  } else {
    state.latestB = event.snapshot;
  }
}

function pointFromFreshState(
  tsMs: number,
  latestA: TickSnapshot | null,
  latestB: TickSnapshot | null,
  aRate: number,
  bRate: number,
  maxStaleMs: number
): SpreadPoint | null {
  if (!latestA || !latestB || !hasFreshSnapshots(tsMs, latestA, latestB, maxStaleMs)) return null;
  return pointFromSnapshots(tsMs, latestA, latestB, aRate, bRate);
}

function hasFreshSnapshots(tsMs: number, a: TickSnapshot, b: TickSnapshot, maxStaleMs: number) {
  return (
    tsMs >= a.tsMs &&
    tsMs >= b.tsMs &&
    tsMs - a.tsMs <= maxStaleMs &&
    tsMs - b.tsMs <= maxStaleMs
  );
}

function normalizeTick(row: RawTickRow | null): TickSnapshot | null {
  if (!row) return null;
  const tsMs = nullableNumber(row.tsMs);
  const bid = nullableNumber(row.bid);
  const ask = nullableNumber(row.ask);
  if (tsMs === null || bid === null || ask === null) return null;

  return {
    tsMs,
    bid,
    ask,
    bidSize: nullableNumber(row.bidSize),
    askSize: nullableNumber(row.askSize),
    bidSizeText: nullableString(row.bidSizeText),
    askSizeText: nullableString(row.askSizeText),
    bidOrderCount: nullableInteger(row.bidOrderCount),
    askOrderCount: nullableInteger(row.askOrderCount),
    mid: nullableNumber(row.mid)
  };
}

function pointFromSnapshots(
  tsMs: number,
  a: TickSnapshot,
  b: TickSnapshot,
  aRate: number,
  bRate: number
): SpreadPoint {
  const aBid = a.bid * aRate;
  const aAsk = a.ask * aRate;
  const bBid = b.bid * bRate;
  const bAsk = b.ask * bRate;
  const aMid = midpoint(a) * aRate;
  const bMid = midpoint(b) * bRate;
  const aToB = aBid - bAsk;
  const bToA = bBid - aAsk;

  return {
    tsMs,
    aBid,
    aAsk,
    aBidSize: a.bidSize,
    aAskSize: a.askSize,
    aBidSizeText: a.bidSizeText,
    aAskSizeText: a.askSizeText,
    aBidOrderCount: a.bidOrderCount,
    aAskOrderCount: a.askOrderCount,
    bBid,
    bAsk,
    bBidSize: b.bidSize,
    bAskSize: b.askSize,
    bBidSizeText: b.bidSizeText,
    bAskSizeText: b.askSizeText,
    bBidOrderCount: b.bidOrderCount,
    bAskOrderCount: b.askOrderCount,
    aMid,
    bMid,
    aToB,
    bToA,
    aToBBp: bAsk === 0 ? null : (aToB / bAsk) * 10000,
    bToABp: aAsk === 0 ? null : (bToA / aAsk) * 10000,
    midDiff: aMid - bMid
  };
}

function midpoint(snapshot: TickSnapshot): number {
  return snapshot.mid ?? (snapshot.bid + snapshot.ask) / 2;
}

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

function resolveGranularity(value: unknown, fromMs: number, toMs: number): SpreadGranularity {
  const rangeMs = toMs - fromMs;
  if (value === 'raw') {
    if (rangeMs > RAW_EXPLICIT_MAX_RANGE_MS) {
      throw new ClickHouseError('Raw tick mode is capped at 6 hours. Use a shorter range or bucketed mode.', 400);
    }
    return 'raw';
  }
  if (value === 'bucket') return 'bucket';
  return 'bucket';
}

function parseBucketSeconds(value: unknown, fromMs: number, toMs: number): number {
  if (typeof value === 'number' && Number.isFinite(value) && value > 0) {
    return clamp(Math.trunc(value), 1, 3600);
  }
  const rangeSeconds = Math.max(1, (toMs - fromMs) / 1000);
  return clamp(Math.ceil(rangeSeconds / TARGET_POINTS), 1, 3600);
}

function maxStaleMsForBucket(bucketSeconds: number): number {
  return clamp(Math.trunc(bucketSeconds * 1000 * 2), 10_000, 60_000);
}

function sampleSecondsForBucket(bucketSeconds: number): number {
  return clamp(Math.trunc(bucketSeconds), 1, 3600);
}

function nullableNumber(value: unknown): number | null {
  if (value === null || value === undefined) return null;
  const parsed = Number(value);
  return Number.isFinite(parsed) ? parsed : null;
}

function nullableBigInt(value: unknown): bigint | null {
  if (value === null || value === undefined) return null;
  try {
    return BigInt(String(value));
  } catch {
    return null;
  }
}

function nullableInteger(value: unknown): number | null {
  const parsed = nullableNumber(value);
  return parsed === null ? null : Math.trunc(parsed);
}

function nullableString(value: unknown): string | null {
  if (value === null || value === undefined) return null;
  const text = String(value).trim();
  return text.length > 0 ? text : null;
}

function clamp(value: number, min: number, max: number): number {
  return Math.min(max, Math.max(min, value));
}

function apiError(error: unknown): Response {
  const status = error instanceof ClickHouseError ? error.status : 500;
  const message = error instanceof Error ? error.message : 'Unknown server error';
  return json({ error: message }, { status });
}
