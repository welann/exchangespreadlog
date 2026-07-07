import { json, type RequestHandler } from '@sveltejs/kit';
import type { QuoteRate, SpreadPoint, SpreadResponse } from '$lib/types';
import { fetchInstruments } from '$lib/server/catalog';
import { ClickHouseError, numericLiteral, queryClickHouse, tickTable } from '$lib/server/clickhouse';
import { resolveRates } from '$lib/server/rates';
import { getTickSchema, tickIdentityWhere } from '$lib/server/tick-schema';

const MAX_RANGE_MS = 31 * 24 * 60 * 60 * 1000;
const TARGET_POINTS = 420;

type SpreadRequest = {
  catalogA?: unknown;
  catalogB?: unknown;
  fromMs?: unknown;
  toMs?: unknown;
  bucketSeconds?: unknown;
  rates?: QuoteRate[];
};

type RawSpreadPoint = {
  tsMs: number | string;
  aBid: number | null;
  aAsk: number | null;
  bBid: number | null;
  bAsk: number | null;
  aMid: number | null;
  bMid: number | null;
  aToB: number | null;
  bToA: number | null;
  aToBBp: number | null;
  bToABp: number | null;
  midDiff: number | null;
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
    const bucketSeconds = parseBucketSeconds(payload.bucketSeconds, fromMs, toMs);
    const tickSchema = await getTickSchema();
    const points = await fetchSpreadPoints({
      instrumentA,
      instrumentB,
      fromMs,
      toMs,
      bucketSeconds,
      aRate,
      bRate,
      tickSchema
    });

    const response: SpreadResponse = {
      meta: {
        fromMs,
        toMs,
        bucketSeconds,
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

async function fetchSpreadPoints(input: {
  instrumentA: SpreadResponse['meta']['instrumentA'];
  instrumentB: SpreadResponse['meta']['instrumentB'];
  fromMs: number;
  toMs: number;
  bucketSeconds: number;
  aRate: number;
  bRate: number;
  tickSchema: Awaited<ReturnType<typeof getTickSchema>>;
}): Promise<SpreadPoint[]> {
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
  b.bid_price * ${bRate} AS bBid,
  b.ask_price * ${bRate} AS bAsk,
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

  return rows.map((row) => ({
    tsMs: Number(row.tsMs),
    aBid: nullableNumber(row.aBid),
    aAsk: nullableNumber(row.aAsk),
    bBid: nullableNumber(row.bBid),
    bAsk: nullableNumber(row.bAsk),
    aMid: nullableNumber(row.aMid),
    bMid: nullableNumber(row.bMid),
    aToB: nullableNumber(row.aToB),
    bToA: nullableNumber(row.bToA),
    aToBBp: nullableNumber(row.aToBBp),
    bToABp: nullableNumber(row.bToABp),
    midDiff: nullableNumber(row.midDiff)
  }));
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

function clamp(value: number, min: number, max: number): number {
  return Math.min(max, Math.max(min, value));
}

function apiError(error: unknown): Response {
  const status = error instanceof ClickHouseError ? error.status : 500;
  const message = error instanceof Error ? error.message : 'Unknown server error';
  return json({ error: message }, { status });
}
