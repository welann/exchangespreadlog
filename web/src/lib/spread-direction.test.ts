import assert from 'node:assert/strict';
import test from 'node:test';

import {
  grossCaptureBp,
  opportunityLegs,
  oppositeDirection
} from './spread-direction.ts';
import type { SpreadPoint } from './types';

function point(
  tsMs: number,
  aBid: number,
  aAsk: number,
  bBid: number,
  bAsk: number
): SpreadPoint {
  const aToB = aBid - bAsk;
  const bToA = bBid - aAsk;
  return {
    tsMs,
    aStateTsMs: tsMs,
    bStateTsMs: tsMs,
    aBid,
    aAsk,
    bBid,
    bAsk,
    aToB,
    bToA,
    aToBBp: (aToB / bAsk) * 10_000,
    bToABp: (bToA / aAsk) * 10_000
  };
}

function swapLegs(value: SpreadPoint): SpreadPoint {
  return {
    tsMs: value.tsMs,
    aStateTsMs: value.bStateTsMs,
    bStateTsMs: value.aStateTsMs,
    aBid: value.bBid,
    aAsk: value.bAsk,
    bBid: value.aBid,
    bAsk: value.aAsk,
    aToB: value.bToA,
    bToA: value.aToB,
    aToBBp: value.bToABp,
    bToABp: value.aToBBp
  };
}

test('maps executable spread directions to their actual buy and sell legs', () => {
  assert.deepEqual(opportunityLegs('aToB', 'A', 'B'), { buy: 'B', sell: 'A' });
  assert.deepEqual(opportunityLegs('bToA', 'A', 'B'), { buy: 'A', sell: 'B' });
});

test('calculates both opening directions with the purchase-leg notional', () => {
  const bToAEntry = point(1_000, 99, 100, 102, 103);
  const bToAClose = point(2_000, 101, 102, 99, 100);
  const aToBEntry = point(1_000, 102, 103, 99, 100);
  const aToBClose = point(2_000, 99, 100, 101, 102);

  assert.equal(grossCaptureBp(bToAEntry, bToAClose, 'bToA'), 300);
  assert.equal(grossCaptureBp(aToBEntry, aToBClose, 'aToB'), 300);
  assert.equal(grossCaptureBp(bToAClose, bToAEntry, 'bToA'), null);
});

test('keeps the same economic trade after swapping A and B', () => {
  const entry = point(1_000, 99, 100, 102, 103);
  const close = point(2_000, 101, 102, 99, 100);
  const original = grossCaptureBp(entry, close, 'bToA');
  const swapped = grossCaptureBp(swapLegs(entry), swapLegs(close), 'aToB');

  assert.equal(oppositeDirection('bToA'), 'aToB');
  assert.equal(swapped, original);
});
