import { json, type RequestHandler } from '@sveltejs/kit';
import type { QuoteRate, SpreadPoint, SpreadResponse } from '$lib/types';
import { fetchInstruments } from '$lib/server/catalog';
import { ClickHouseError, numericLiteral, queryClickHouse, tickTable } from '$lib/server/clickhouse';
import { resolveRates } from '$lib/server/rates';
import { getTickSchema, tickIdentityWhere } from '$lib/server/tick-schema';

const MAX_RANGE_MS = 31 * 24 * 60 * 60 * 1000;
const TARGET_POINTS = 420;
const RAW_EXPLICIT_MAX_RANGE_MS = 6 * 60 * 60 * 1000;
const MAX_RAW_TICK_ROWS = 100_000;
const MAX_BUCKET_TICK_ROWS = 500_000;

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

type BookRowsInput = Omit<BaseSpreadInput, 'aRate' | 'bRate' | 'maxStaleMs'> & {
  maxRows: number;
};

type BookRowsResult = {
  seedRows: RawTickRow[];
  tickRows: RawTickRow[];
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
    const result =
      granularity === 'raw'
        ? await fetchRawSpreadPoints({
            instrumentA,
            instrumentB,
            fromMs,
            toMs,
            aRate,
            bRate,
            maxStaleMs,
            tickSchema
          })
        : await fetchBucketedSpreadPoints({
            instrumentA,
            instrumentB,
            fromMs,
            toMs,
            bucketSeconds,
            aRate,
            bRate,
            maxStaleMs,
            tickSchema
          });

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
    return json(response);
  } catch (error) {
    return apiError(error);
  }
};

async function fetchSelectedInstruments(catalogA: string, catalogB: string) {
  const instruments = await fetchInstruments([catalogA, catalogB]);
  const instrumentA = instruments.find((instrument) => instrument.catalogId === catalogA);
  const instrumentB = instruments.find((instrument) => instrument.catalogId === catalogB);
  if (!instrumentA || !instrumentB) {
    throw new ClickHouseError('Selected instrument was not found in instrument_catalog', 400);
  }
  return [instrumentA, instrumentB] as const;
}

async function fetchRawSpreadPoints(input: BaseSpreadInput): Promise<SpreadQueryResult> {
  const { seedRows, tickRows } = await fetchBookRows({
    instrumentA: input.instrumentA,
    instrumentB: input.instrumentB,
    fromMs: input.fromMs,
    toMs: input.toMs,
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
  const { seedRows, tickRows } = await fetchBookRows({
    instrumentA: input.instrumentA,
    instrumentB: input.instrumentB,
    fromMs: input.fromMs,
    toMs: input.toMs,
    tickSchema: input.tickSchema,
    maxRows: MAX_BUCKET_TICK_ROWS
  });

  return {
    points: buildBucketExtremeSpreadPoints(
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

async function fetchBookRows(input: BookRowsInput): Promise<BookRowsResult> {
  const whereA = tickIdentityWhere(input.tickSchema, input.instrumentA, 'a_ticks');
  const whereB = tickIdentityWhere(input.tickSchema, input.instrumentB, 'b_ticks');
  const seedWhereA = tickIdentityWhere(input.tickSchema, input.instrumentA, 'a_seed');
  const seedWhereB = tickIdentityWhere(input.tickSchema, input.instrumentB, 'b_seed');
  const limit = input.maxRows + 1;

  const [seedRows, tickRows] = await Promise.all([
    queryClickHouse<RawTickRow>(`
WITH
  fromUnixTimestamp64Milli(${numericLiteral(input.fromMs)}) AS start_time
SELECT *
FROM
(
  SELECT
    'a' AS side,
    toUnixTimestamp64Milli(argMax(a_seed.recv_time, a_seed.recv_ts_ns)) AS tsMs,
    max(a_seed.recv_ts_ns) AS tsNs,
    argMax(a_seed.bid_price, a_seed.recv_ts_ns) AS bid,
    argMax(a_seed.ask_price, a_seed.recv_ts_ns) AS ask,
    argMax(a_seed.bid_size, a_seed.recv_ts_ns) AS bidSize,
    argMax(a_seed.ask_size, a_seed.recv_ts_ns) AS askSize,
    argMax(a_seed.bid_size_text, a_seed.recv_ts_ns) AS bidSizeText,
    argMax(a_seed.ask_size_text, a_seed.recv_ts_ns) AS askSizeText,
    argMax(a_seed.bid_order_count, a_seed.recv_ts_ns) AS bidOrderCount,
    argMax(a_seed.ask_order_count, a_seed.recv_ts_ns) AS askOrderCount,
    argMax(a_seed.mid, a_seed.recv_ts_ns) AS mid
  FROM ${tickTable()} AS a_seed
  WHERE ${seedWhereA}
    AND a_seed.recv_time < start_time
    AND a_seed.bid_price IS NOT NULL
    AND a_seed.ask_price IS NOT NULL
  HAVING count() > 0
  UNION ALL
  SELECT
    'b' AS side,
    toUnixTimestamp64Milli(argMax(b_seed.recv_time, b_seed.recv_ts_ns)) AS tsMs,
    max(b_seed.recv_ts_ns) AS tsNs,
    argMax(b_seed.bid_price, b_seed.recv_ts_ns) AS bid,
    argMax(b_seed.ask_price, b_seed.recv_ts_ns) AS ask,
    argMax(b_seed.bid_size, b_seed.recv_ts_ns) AS bidSize,
    argMax(b_seed.ask_size, b_seed.recv_ts_ns) AS askSize,
    argMax(b_seed.bid_size_text, b_seed.recv_ts_ns) AS bidSizeText,
    argMax(b_seed.ask_size_text, b_seed.recv_ts_ns) AS askSizeText,
    argMax(b_seed.bid_order_count, b_seed.recv_ts_ns) AS bidOrderCount,
    argMax(b_seed.ask_order_count, b_seed.recv_ts_ns) AS askOrderCount,
    argMax(b_seed.mid, b_seed.recv_ts_ns) AS mid
  FROM ${tickTable()} AS b_seed
  WHERE ${seedWhereB}
    AND b_seed.recv_time < start_time
    AND b_seed.bid_price IS NOT NULL
    AND b_seed.ask_price IS NOT NULL
  HAVING count() > 0
)
FORMAT JSONEachRow
`),
    queryClickHouse<RawTickRow>(`
WITH
  fromUnixTimestamp64Milli(${numericLiteral(input.fromMs)}) AS start_time,
  fromUnixTimestamp64Milli(${numericLiteral(input.toMs)}) AS end_time
SELECT *
FROM
(
  SELECT
    'a' AS side,
    toUnixTimestamp64Milli(a_ticks.recv_time) AS tsMs,
    a_ticks.recv_ts_ns AS tsNs,
    a_ticks.bid_price AS bid,
    a_ticks.ask_price AS ask,
    a_ticks.bid_size AS bidSize,
    a_ticks.ask_size AS askSize,
    a_ticks.bid_size_text AS bidSizeText,
    a_ticks.ask_size_text AS askSizeText,
    a_ticks.bid_order_count AS bidOrderCount,
    a_ticks.ask_order_count AS askOrderCount,
    a_ticks.mid AS mid
  FROM ${tickTable()} AS a_ticks
  WHERE ${whereA}
    AND a_ticks.recv_time >= start_time
    AND a_ticks.recv_time <= end_time
    AND a_ticks.bid_price IS NOT NULL
    AND a_ticks.ask_price IS NOT NULL
  UNION ALL
  SELECT
    'b' AS side,
    toUnixTimestamp64Milli(b_ticks.recv_time) AS tsMs,
    b_ticks.recv_ts_ns AS tsNs,
    b_ticks.bid_price AS bid,
    b_ticks.ask_price AS ask,
    b_ticks.bid_size AS bidSize,
    b_ticks.ask_size AS askSize,
    b_ticks.bid_size_text AS bidSizeText,
    b_ticks.ask_size_text AS askSizeText,
    b_ticks.bid_order_count AS bidOrderCount,
    b_ticks.ask_order_count AS askOrderCount,
    b_ticks.mid AS mid
  FROM ${tickTable()} AS b_ticks
  WHERE ${whereB}
    AND b_ticks.recv_time >= start_time
    AND b_ticks.recv_time <= end_time
    AND b_ticks.bid_price IS NOT NULL
    AND b_ticks.ask_price IS NOT NULL
)
ORDER BY tsMs ASC, tsNs ASC, side ASC
LIMIT ${limit}
FORMAT JSONEachRow
`)
  ]);

  if (tickRows.length > input.maxRows) {
    throw new ClickHouseError(
      `Spread query is capped at ${input.maxRows} source rows. Reduce the time range or use a coarser window.`,
      400
    );
  }

  return { seedRows, tickRows };
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

function buildBucketExtremeSpreadPoints(
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
    const includeEnd = bucketEnd >= toMs;
    let bucketPoint = pointFromFreshState(bucketStart, state.latestA, state.latestB, aRate, bRate, maxStaleMs);

    while (eventIndex < events.length) {
      const event = events[eventIndex];
      const eventTs = event.snapshot.tsMs;
      if (eventTs < bucketStart) {
        applyTickEvent(state, event);
        eventIndex += 1;
        continue;
      }
      if (eventTs > bucketEnd || (!includeEnd && eventTs >= bucketEnd)) break;

      applyTickEvent(state, event);
      const eventPoint = pointFromFreshState(eventTs, state.latestA, state.latestB, aRate, bRate, maxStaleMs);
      bucketPoint = moreExtremePoint(bucketPoint, eventPoint);
      eventIndex += 1;
    }

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

function moreExtremePoint(current: SpreadPoint | null, candidate: SpreadPoint | null) {
  if (!candidate) return current;
  if (!current) return candidate;

  const currentRank = pointExtremeRank(current);
  const candidateRank = pointExtremeRank(candidate);
  if (candidateRank.tier !== currentRank.tier) {
    return candidateRank.tier > currentRank.tier ? candidate : current;
  }
  if (candidateRank.value !== currentRank.value) {
    return candidateRank.value > currentRank.value ? candidate : current;
  }
  return candidate.tsMs >= current.tsMs ? candidate : current;
}

function pointExtremeRank(point: SpreadPoint) {
  const values = [point.aToBBp, point.bToABp].filter(
    (value): value is number => value !== null && Number.isFinite(value)
  );
  if (values.length === 0) return { tier: -1, value: Number.NEGATIVE_INFINITY };

  const positives = values.filter((value) => value > 0);
  if (positives.length > 0) {
    return { tier: 1, value: Math.max(...positives) };
  }
  return { tier: 0, value: Math.max(...values.map((value) => Math.abs(value))) };
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
  return clamp(Math.trunc(bucketSeconds * 1000 * 4), 30_000, 5 * 60_000);
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
