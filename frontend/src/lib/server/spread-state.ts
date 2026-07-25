export type RawTickRow = {
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

export type ComputedSpreadPoint = {
  id: string;
  tsMs: number;
  aStateTsMs: number;
  bStateTsMs: number;
  aBid: number;
  aAsk: number;
  aBidSize: number | null;
  aAskSize: number | null;
  aBidSizeText: string | null;
  aAskSizeText: string | null;
  aBidOrderCount: number | null;
  aAskOrderCount: number | null;
  bBid: number;
  bAsk: number;
  bBidSize: number | null;
  bAskSize: number | null;
  bBidSizeText: string | null;
  bAskSizeText: string | null;
  bBidOrderCount: number | null;
  bAskOrderCount: number | null;
  aMid: number;
  bMid: number;
  aToB: number;
  bToA: number;
  aToBBp: number | null;
  bToABp: number | null;
  midDiff: number;
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

type BookState = {
  latestA: TickSnapshot | null;
  latestB: TickSnapshot | null;
};

/**
 * Replays every BBO update as a state transition. The venue that did not
 * update keeps its last valid book until a newer snapshot replaces it.
 */
export function buildEventSpreadPoints(
  seedRows: RawTickRow[],
  tickRows: RawTickRow[],
  aRate: number,
  bRate: number,
  maxStaleMs = 120_000
): ComputedSpreadPoint[] {
  const events = normalizeTickEvents(tickRows);
  const state = initialBookState(seedRows);
  const points: ComputedSpreadPoint[] = [];

  for (const event of events) {
    applyTickEvent(state, event);
    const point = pointFromCurrentState(
      `raw:${event.side}:${event.tsNs}`,
      event.snapshot.tsMs,
      state.latestA,
      state.latestB,
      aRate,
      bRate,
      maxStaleMs
    );
    if (point) points.push(point);
  }

  return points;
}

/**
 * Materializes the latest A/B state at every bucket boundary. A quiet market
 * therefore produces repeated book state rather than disappearing merely
 * because its best bid and ask did not change.
 */
export function buildBucketSnapshotSpreadPoints(
  seedRows: RawTickRow[],
  tickRows: RawTickRow[],
  fromMs: number,
  toMs: number,
  bucketSeconds: number,
  aRate: number,
  bRate: number,
  maxStaleMs = 120_000
): ComputedSpreadPoint[] {
  const bucketMs = Math.max(1, Math.trunc(bucketSeconds)) * 1000;
  const events = normalizeTickEvents(tickRows);
  const state = initialBookState(seedRows);
  const points: ComputedSpreadPoint[] = [];
  let eventIndex = 0;

  for (let bucketStart = fromMs; bucketStart < toMs; bucketStart += bucketMs) {
    const bucketEnd = Math.min(toMs, bucketStart + bucketMs);

    while (eventIndex < events.length) {
      const event = events[eventIndex];
      if (event.snapshot.tsMs > bucketEnd) break;

      applyTickEvent(state, event);
      eventIndex += 1;
    }

    const bucketPoint = pointFromCurrentState(
      `bucket:${bucketMs}:${bucketEnd}`,
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

function initialBookState(seedRows: RawTickRow[]): BookState {
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

function applyTickEvent(state: BookState, event: TickEvent) {
  if (event.side === 'a') {
    state.latestA = event.snapshot;
  } else {
    state.latestB = event.snapshot;
  }
}

function pointFromCurrentState(
  id: string,
  tsMs: number,
  latestA: TickSnapshot | null,
  latestB: TickSnapshot | null,
  aRate: number,
  bRate: number,
  maxStaleMs: number
): ComputedSpreadPoint | null {
  if (
    !latestA ||
    !latestB ||
    tsMs < latestA.tsMs ||
    tsMs < latestB.tsMs ||
    tsMs - latestA.tsMs > maxStaleMs ||
    tsMs - latestB.tsMs > maxStaleMs
  ) {
    return null;
  }
  return pointFromSnapshots(id, tsMs, latestA, latestB, aRate, bRate);
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
  id: string,
  tsMs: number,
  a: TickSnapshot,
  b: TickSnapshot,
  aRate: number,
  bRate: number
): ComputedSpreadPoint {
  const aBid = a.bid * aRate;
  const aAsk = a.ask * aRate;
  const bBid = b.bid * bRate;
  const bAsk = b.ask * bRate;
  const aMid = midpoint(a) * aRate;
  const bMid = midpoint(b) * bRate;
  const aToB = aBid - bAsk;
  const bToA = bBid - aAsk;

  return {
    id,
    tsMs,
    aStateTsMs: a.tsMs,
    bStateTsMs: b.tsMs,
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
