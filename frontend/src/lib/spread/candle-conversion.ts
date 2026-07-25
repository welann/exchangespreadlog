import type { Instrument, SpreadPoint } from '$lib/types';

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
  final_mid: number | null;
  final_recv_ts_ns: number | string | null;
}

export function convertCandleRowsToSpreadPoints(
  candleRows: RawCandleRow[],
  fromMs: number,
  toMs: number,
  bucketMs: number,
  _instrumentA: Instrument,
  _instrumentB: Instrument,
  aRate: number,
  bRate: number,
  maxStaleMs = 120_000
): SpreadPoint[] {
  const sorted = [...candleRows].sort((left, right) => {
    const tsL = new Date(left.bucket_time).getTime();
    const tsR = new Date(right.bucket_time).getTime();
    if (tsL !== tsR) return tsL - tsR;
    return left.side.localeCompare(right.side);
  });
  let lastA: RawCandleRow | undefined;
  let lastB: RawCandleRow | undefined;
  const alignedFromMs = Math.ceil(fromMs / bucketMs) * bucketMs;
  const alignedToMs = Math.floor(toMs / bucketMs) * bucketMs;
  const byBucket = new Map<number, { a?: RawCandleRow; b?: RawCandleRow }>();

  for (const row of sorted) {
    const ts = new Date(row.bucket_time).getTime();
    if (!Number.isFinite(ts)) continue;
    if (ts < alignedFromMs) {
      if (row.side === 'a') lastA = row;
      else if (row.side === 'b') lastB = row;
    } else if (ts < alignedToMs) {
      const entry = byBucket.get(ts) ?? {};
      if (row.side === 'a') entry.a = row;
      else if (row.side === 'b') entry.b = row;
      byBucket.set(ts, entry);
    }
  }

  const points: SpreadPoint[] = [];
  for (let bucketStart = alignedFromMs; bucketStart < alignedToMs; bucketStart += bucketMs) {
    const entry = byBucket.get(bucketStart);
    if (entry?.a) lastA = entry.a;
    if (entry?.b) lastB = entry.b;
    if (!lastA || !lastB) continue;
    const point = buildSpreadPoint(
      bucketStart + bucketMs,
      bucketMs,
      lastA,
      lastB,
      aRate,
      bRate,
      maxStaleMs
    );
    if (point) points.push(point);
  }
  return points;
}

function buildSpreadPoint(
  tsMs: number,
  bucketMs: number,
  a: RawCandleRow,
  b: RawCandleRow,
  aRate: number,
  bRate: number,
  maxStaleMs: number
): SpreadPoint | null {
  const aBidRaw = positiveFinite(a.final_bid_price);
  const aAskRaw = positiveFinite(a.final_ask_price);
  const bBidRaw = positiveFinite(b.final_bid_price);
  const bAskRaw = positiveFinite(b.final_ask_price);
  const aStateTsMs = recvTsMs(a.final_recv_ts_ns);
  const bStateTsMs = recvTsMs(b.final_recv_ts_ns);
  if (
    aBidRaw === null ||
    aAskRaw === null ||
    bBidRaw === null ||
    bAskRaw === null ||
    aBidRaw > aAskRaw ||
    bBidRaw > bAskRaw ||
    aStateTsMs === null ||
    bStateTsMs === null ||
    tsMs - aStateTsMs > maxStaleMs ||
    tsMs - bStateTsMs > maxStaleMs
  ) {
    return null;
  }

  const aBid = aBidRaw * aRate;
  const aAsk = aAskRaw * aRate;
  const bBid = bBidRaw * bRate;
  const bAsk = bAskRaw * bRate;
  const aMid = (positiveFinite(a.final_mid) ?? (aBidRaw + aAskRaw) / 2) * aRate;
  const bMid = (positiveFinite(b.final_mid) ?? (bBidRaw + bAskRaw) / 2) * bRate;
  const aToB = aBid - bAsk;
  const bToA = bBid - aAsk;

  return {
    id: `candle:${bucketMs}:${tsMs}`,
    tsMs,
    aStateTsMs,
    bStateTsMs,
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
    aToBBp: (aToB / bAsk) * 10000,
    bToABp: (bToA / aAsk) * 10000,
    midDiff: aMid - bMid
  };
}

function positiveFinite(value: unknown): number | null {
  const parsed = Number(value);
  return Number.isFinite(parsed) && parsed > 0 ? parsed : null;
}

function recvTsMs(value: unknown): number | null {
  if (value === null || value === undefined) return null;
  try {
    const ns = BigInt(String(value));
    return Number(ns / 1_000_000n);
  } catch {
    return null;
  }
}
