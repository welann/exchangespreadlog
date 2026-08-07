import type { SpreadPoint } from './types';

export type SpreadDirection = 'aToB' | 'bToA';
export type SpreadLeg = 'A' | 'B';

export function directionLegs(
  direction: SpreadDirection
): { buy: SpreadLeg; sell: SpreadLeg } {
  return direction === 'aToB' ? { buy: 'B', sell: 'A' } : { buy: 'A', sell: 'B' };
}

export function directionLabel(direction: SpreadDirection): string {
  const { buy, sell } = directionLegs(direction);
  return `BUY ${buy} · SELL ${sell}`;
}

export function oppositeDirection(direction: SpreadDirection): SpreadDirection {
  return direction === 'aToB' ? 'bToA' : 'aToB';
}

export function directionBp(point: SpreadPoint, direction: SpreadDirection): number {
  return direction === 'aToB' ? point.aToBBp : point.bToABp;
}

export function directionQuote(point: SpreadPoint, direction: SpreadDirection): number {
  return direction === 'aToB' ? point.aToB : point.bToA;
}

export function openingCost(point: SpreadPoint, direction: SpreadDirection): number {
  return direction === 'aToB' ? point.bAsk : point.aAsk;
}

export function grossCaptureBp(
  entry: SpreadPoint,
  close: SpreadPoint,
  openDirection: SpreadDirection
): number | null {
  const cost = openingCost(entry, openDirection);
  if (close.tsMs < entry.tsMs || !Number.isFinite(cost) || cost <= 0) return null;

  const grossQuote =
    directionQuote(entry, openDirection) +
    directionQuote(close, oppositeDirection(openDirection));
  return Number.isFinite(grossQuote) ? (grossQuote / cost) * 10_000 : null;
}

export function opportunityLegs<T>(
  direction: SpreadDirection,
  first: T,
  second: T
): { buy: T; sell: T } {
  return direction === 'aToB'
    ? { buy: second, sell: first }
    : { buy: first, sell: second };
}
