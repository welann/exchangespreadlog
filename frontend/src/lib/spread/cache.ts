import type { QuoteRate, SpreadResponse } from '$lib/types';

export class TimedLruCache<T> {
  private readonly values = new Map<string, { value: T; loadedAt: number }>();
  private readonly maxEntries: number;
  private readonly ttlMs: number;

  constructor(maxEntries: number, ttlMs: number) {
    this.maxEntries = maxEntries;
    this.ttlMs = ttlMs;
  }

  get(key: string, now = Date.now()): T | null {
    const cached = this.values.get(key);
    if (!cached) return null;
    if (now - cached.loadedAt > this.ttlMs) {
      this.values.delete(key);
      return null;
    }
    this.values.delete(key);
    this.values.set(key, cached);
    return cached.value;
  }

  set(key: string, value: T, now = Date.now()): void {
    this.values.delete(key);
    this.values.set(key, { value, loadedAt: now });
    while (this.values.size > this.maxEntries) {
      const oldest = this.values.keys().next().value;
      if (typeof oldest !== 'string') return;
      this.values.delete(oldest);
    }
  }

  get size(): number {
    return this.values.size;
  }
}

export type SpreadCacheInput = {
  catalogA: string;
  catalogB: string;
  storageA?: string;
  storageB?: string;
  fromMs: number;
  toMs: number;
  targetQuote?: string;
  precision?: unknown;
  bucketSeconds?: unknown;
  rates?: QuoteRate[];
};

export function spreadCacheKey(input: SpreadCacheInput): string {
  const rates = [...(input.rates ?? [])]
    .map((rate) => ({
      from: rate.from.trim().toUpperCase(),
      to: rate.to.trim().toUpperCase(),
      rate: rate.rate.trim()
    }))
    .sort((left, right) => `${left.from}:${left.to}`.localeCompare(`${right.from}:${right.to}`));
  return JSON.stringify({
    version: 3,
    catalogA: input.catalogA,
    catalogB: input.catalogB,
    storageA: input.storageA ?? input.catalogA,
    storageB: input.storageB ?? input.catalogB,
    fromMs: Math.trunc(input.fromMs),
    toMs: Math.trunc(input.toMs),
    precision: input.precision ?? 'auto',
    bucketSeconds: input.bucketSeconds ?? null,
    targetQuote: input.targetQuote?.trim().toUpperCase() ?? '',
    rates
  });
}

export function mergeSpreadResponses(
  base: SpreadResponse,
  delta: SpreadResponse,
  maxPoints = 5_000
): SpreadResponse {
  const byId = new Map(base.points.map((point) => [point.id, point]));
  delta.points.forEach((point) => byId.set(point.id, point));
  const points = [...byId.values()]
    .sort((left, right) => left.tsMs - right.tsMs || left.id.localeCompare(right.id))
    .slice(-maxPoints);
  return {
    meta: {
      ...base.meta,
      coverage: {
        fromMs: Math.min(base.meta.coverage.fromMs, delta.meta.coverage.fromMs),
        toMs: Math.max(base.meta.coverage.toMs, delta.meta.coverage.toMs),
        complete: base.meta.coverage.complete && delta.meta.coverage.complete
      },
      nextCursor: delta.meta.nextCursor ?? base.meta.nextCursor,
      sourceRows: base.meta.sourceRows + delta.meta.sourceRows
    },
    points
  };
}
