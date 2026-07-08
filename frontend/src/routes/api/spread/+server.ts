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

type RawSpreadPoint = {
  tsMs: number | string;
  aBid: number | null;
  aAsk: number | null;
  aBidSize: number | null;
  aAskSize: number | null;
  aBidSizeText: string | null;
  aAskSizeText: string | null;
  aBidOrderCount: number | string | null;
  aAskOrderCount: number | string | null;
  bBid: number | null;
  bAsk: number | null;
  bBidSize: number | null;
  bAskSize: number | null;
  bBidSizeText: string | null;
  bAskSizeText: string | null;
  bBidOrderCount: number | string | null;
  bAskOrderCount: number | string | null;
  aMid: number | null;
  bMid: number | null;
  aToB: number | null;
  bToA: number | null;
  aToBBp: number | null;
  bToABp: number | null;
  midDiff: number | null;
};

type RawTickRow = {
  side: 'a' | 'b';
  tsMs: number | string;
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

async function fetchRawSpreadPoints(input: {
  instrumentA: SpreadResponse['meta']['instrumentA'];
  instrumentB: SpreadResponse['meta']['instrumentB'];
  fromMs: number;
  toMs: number;
  aRate: number;
  bRate: number;
  tickSchema: Awaited<ReturnType<typeof getTickSchema>>;
}): Promise<SpreadQueryResult> {
  const whereA = tickIdentityWhere(input.tickSchema, input.instrumentA, 'a_ticks');
  const whereB = tickIdentityWhere(input.tickSchema, input.instrumentB, 'b_ticks');
  const seedWhereA = tickIdentityWhere(input.tickSchema, input.instrumentA, 'a_seed');
  const seedWhereB = tickIdentityWhere(input.tickSchema, input.instrumentB, 'b_seed');
  const limit = MAX_RAW_TICK_ROWS + 1;

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
ORDER BY tsMs ASC
LIMIT ${limit}
FORMAT JSONEachRow
`)
  ]);

  if (tickRows.length > MAX_RAW_TICK_ROWS) {
    throw new ClickHouseError(
      `Raw tick mode is capped at ${MAX_RAW_TICK_ROWS} source rows. Reduce the time range or use the bucketed view.`,
      400
    );
  }

  let latestA = normalizeTick(seedRows.find((row) => row.side === 'a') ?? null);
  let latestB = normalizeTick(seedRows.find((row) => row.side === 'b') ?? null);
  const points: SpreadPoint[] = [];

  for (const row of tickRows) {
    const snapshot = normalizeTick(row);
    if (!snapshot) continue;
    if (row.side === 'a') {
      latestA = snapshot;
    } else {
      latestB = snapshot;
    }

    if (latestA && latestB) {
      points.push(pointFromSnapshots(snapshot.tsMs, latestA, latestB, input.aRate, input.bRate));
    }
  }

  return { points, sourceRows: tickRows.length };
}

async function fetchBucketedSpreadPoints(input: {
  instrumentA: SpreadResponse['meta']['instrumentA'];
  instrumentB: SpreadResponse['meta']['instrumentB'];
  fromMs: number;
  toMs: number;
  bucketSeconds: number;
  aRate: number;
  bRate: number;
  tickSchema: Awaited<ReturnType<typeof getTickSchema>>;
}): Promise<SpreadQueryResult> {
  const aRate = numericLiteral(input.aRate);
  const bRate = numericLiteral(input.bRate);
  const bucketSeconds = Math.trunc(input.bucketSeconds);
  const whereA = tickIdentityWhere(input.tickSchema, input.instrumentA, 'a_ticks');
  const whereB = tickIdentityWhere(input.tickSchema, input.instrumentB, 'b_ticks');

  const rows = await queryClickHouse<RawSpreadPoint>(`
WITH
  fromUnixTimestamp64Milli(${numericLiteral(input.fromMs)}) AS start_time,
  fromUnixTimestamp64Milli(${numericLiteral(input.toMs)}) AS end_time
SELECT
  toUnixTimestamp(a.bucket) * 1000 AS tsMs,
  a.bid_price * ${aRate} AS aBid,
  a.ask_price * ${aRate} AS aAsk,
  a.bid_size AS aBidSize,
  a.ask_size AS aAskSize,
  a.bid_size_text AS aBidSizeText,
  a.ask_size_text AS aAskSizeText,
  a.bid_order_count AS aBidOrderCount,
  a.ask_order_count AS aAskOrderCount,
  b.bid_price * ${bRate} AS bBid,
  b.ask_price * ${bRate} AS bAsk,
  b.bid_size AS bBidSize,
  b.ask_size AS bAskSize,
  b.bid_size_text AS bBidSizeText,
  b.ask_size_text AS bAskSizeText,
  b.bid_order_count AS bBidOrderCount,
  b.ask_order_count AS bAskOrderCount,
  a.mid * ${aRate} AS aMid,
  b.mid * ${bRate} AS bMid,
  (a.bid_price * ${aRate}) - (b.ask_price * ${bRate}) AS aToB,
  (b.bid_price * ${bRate}) - (a.ask_price * ${aRate}) AS bToA,
  if((b.ask_price * ${bRate}) = 0, NULL, ((a.bid_price * ${aRate}) - (b.ask_price * ${bRate})) / (b.ask_price * ${bRate}) * 10000) AS aToBBp,
  if((a.ask_price * ${aRate}) = 0, NULL, ((b.bid_price * ${bRate}) - (a.ask_price * ${aRate})) / (a.ask_price * ${aRate}) * 10000) AS bToABp,
  (a.mid * ${aRate}) - (b.mid * ${bRate}) AS midDiff
FROM
(
  SELECT
    toStartOfInterval(a_ticks.recv_time, INTERVAL ${bucketSeconds} SECOND) AS bucket,
    argMax(a_ticks.bid_price, a_ticks.recv_ts_ns) AS bid_price,
    argMax(a_ticks.ask_price, a_ticks.recv_ts_ns) AS ask_price,
    argMax(a_ticks.bid_size, a_ticks.recv_ts_ns) AS bid_size,
    argMax(a_ticks.ask_size, a_ticks.recv_ts_ns) AS ask_size,
    argMax(a_ticks.bid_size_text, a_ticks.recv_ts_ns) AS bid_size_text,
    argMax(a_ticks.ask_size_text, a_ticks.recv_ts_ns) AS ask_size_text,
    argMax(a_ticks.bid_order_count, a_ticks.recv_ts_ns) AS bid_order_count,
    argMax(a_ticks.ask_order_count, a_ticks.recv_ts_ns) AS ask_order_count,
    argMax(a_ticks.mid, a_ticks.recv_ts_ns) AS mid
  FROM ${tickTable()} AS a_ticks
  WHERE ${whereA}
    AND a_ticks.recv_time >= start_time
    AND a_ticks.recv_time <= end_time
    AND a_ticks.bid_price IS NOT NULL
    AND a_ticks.ask_price IS NOT NULL
  GROUP BY bucket
) AS a
INNER JOIN
(
  SELECT
    toStartOfInterval(b_ticks.recv_time, INTERVAL ${bucketSeconds} SECOND) AS bucket,
    argMax(b_ticks.bid_price, b_ticks.recv_ts_ns) AS bid_price,
    argMax(b_ticks.ask_price, b_ticks.recv_ts_ns) AS ask_price,
    argMax(b_ticks.bid_size, b_ticks.recv_ts_ns) AS bid_size,
    argMax(b_ticks.ask_size, b_ticks.recv_ts_ns) AS ask_size,
    argMax(b_ticks.bid_size_text, b_ticks.recv_ts_ns) AS bid_size_text,
    argMax(b_ticks.ask_size_text, b_ticks.recv_ts_ns) AS ask_size_text,
    argMax(b_ticks.bid_order_count, b_ticks.recv_ts_ns) AS bid_order_count,
    argMax(b_ticks.ask_order_count, b_ticks.recv_ts_ns) AS ask_order_count,
    argMax(b_ticks.mid, b_ticks.recv_ts_ns) AS mid
  FROM ${tickTable()} AS b_ticks
  WHERE ${whereB}
    AND b_ticks.recv_time >= start_time
    AND b_ticks.recv_time <= end_time
    AND b_ticks.bid_price IS NOT NULL
    AND b_ticks.ask_price IS NOT NULL
  GROUP BY bucket
) AS b ON a.bucket = b.bucket
ORDER BY a.bucket ASC
FORMAT JSONEachRow
`);

  return {
    points: rows.map((row) => ({
      tsMs: Number(row.tsMs),
      aBid: nullableNumber(row.aBid),
      aAsk: nullableNumber(row.aAsk),
      aBidSize: nullableNumber(row.aBidSize),
      aAskSize: nullableNumber(row.aAskSize),
      aBidSizeText: nullableString(row.aBidSizeText),
      aAskSizeText: nullableString(row.aAskSizeText),
      aBidOrderCount: nullableInteger(row.aBidOrderCount),
      aAskOrderCount: nullableInteger(row.aAskOrderCount),
      bBid: nullableNumber(row.bBid),
      bAsk: nullableNumber(row.bAsk),
      bBidSize: nullableNumber(row.bBidSize),
      bAskSize: nullableNumber(row.bAskSize),
      bBidSizeText: nullableString(row.bBidSizeText),
      bAskSizeText: nullableString(row.bAskSizeText),
      bBidOrderCount: nullableInteger(row.bBidOrderCount),
      bAskOrderCount: nullableInteger(row.bAskOrderCount),
      aMid: nullableNumber(row.aMid),
      bMid: nullableNumber(row.bMid),
      aToB: nullableNumber(row.aToB),
      bToA: nullableNumber(row.bToA),
      aToBBp: nullableNumber(row.aToBBp),
      bToABp: nullableNumber(row.bToABp),
      midDiff: nullableNumber(row.midDiff)
    })),
    sourceRows: rows.length
  };
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

function nullableNumber(value: unknown): number | null {
  if (value === null || value === undefined) return null;
  const parsed = Number(value);
  return Number.isFinite(parsed) ? parsed : null;
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
