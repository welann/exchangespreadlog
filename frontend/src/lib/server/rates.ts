import type { QuoteRate } from '$lib/types';
import { ClickHouseError } from './clickhouse';

type NormalizedRate = {
  from: string;
  to: string;
  rate: number;
};

export type RateResolution = {
  targetQuote: string;
  aRate: number;
  bRate: number;
};

export function resolveRates(
  quoteA: string,
  quoteB: string,
  rawRates: QuoteRate[] | undefined
): RateResolution {
  const rates = normalizeRates(rawRates ?? []);
  const targetQuote = commonQuote(quoteA, quoteB, rates);
  if (!targetQuote) {
    throw new ClickHouseError(`No quote conversion path for ${quoteA} vs ${quoteB}`, 400);
  }

  const aRate = rateFor(quoteA, targetQuote, rates);
  const bRate = rateFor(quoteB, targetQuote, rates);
  if (aRate == null || bRate == null) {
    throw new ClickHouseError(`Missing quote rate into ${targetQuote}`, 400);
  }

  return { targetQuote, aRate, bRate };
}

function normalizeRates(rawRates: QuoteRate[]): NormalizedRate[] {
  return rawRates
    .map((rate) => ({
      from: rate.from.trim().toUpperCase(),
      to: rate.to.trim().toUpperCase(),
      rate: Number(rate.rate)
    }))
    .filter((rate) => rate.from && rate.to)
    .map((rate) => {
      if (!Number.isFinite(rate.rate) || rate.rate <= 0) {
        throw new ClickHouseError(`Invalid quote rate ${rate.from}->${rate.to}`, 400);
      }
      return rate;
    });
}

function commonQuote(
  first: string,
  second: string,
  rates: NormalizedRate[]
): string | undefined {
  const a = first.toUpperCase();
  const b = second.toUpperCase();
  if (a === b) return a;
  if (rateFor(a, b, rates) != null) return b;
  if (rateFor(b, a, rates) != null) return a;

  const firstTargets = convertibleTargets(a, rates);
  const secondTargets = convertibleTargets(b, rates);
  return [...firstTargets].filter((target) => secondTargets.has(target)).sort()[0];
}

function rateFor(from: string, to: string, rates: NormalizedRate[]): number | undefined {
  const a = from.toUpperCase();
  const b = to.toUpperCase();
  if (a === b) return 1;
  return rates.find((rate) => rate.from === a && rate.to === b)?.rate;
}

function convertibleTargets(asset: string, rates: NormalizedRate[]): Set<string> {
  return new Set([asset, ...rates.filter((rate) => rate.from === asset).map((rate) => rate.to)]);
}
