export type QuoteRate = {
  from: string;
  to: string;
  rate: string;
};

export type SpreadRequestedPrecision = 'auto' | 'raw' | 'bucket' | 'candle';
export type SpreadSource = 'raw' | 'bucket' | 'candle';
export type SpreadGranularity = 'raw' | 'bucket' | '1s' | '1m' | '5m' | '15m' | '1h';

export type Instrument = {
  catalogId: string;
  venueInstanceId: string;
  instrumentId: string;
  rawSymbol: string;
  baseAsset: string;
  quoteAsset: string;
  status: string;
  latestRecvMs: number | null;
  label: string;
};

export type Market = {
  baseAsset: string;
  instruments: Instrument[];
};

export type SpreadPoint = {
  id: string;
  tsMs: number;
  aStateTsMs: number;
  bStateTsMs: number;
  aBid: number | null;
  aAsk: number | null;
  aBidSize: number | null;
  aAskSize: number | null;
  aBidSizeText: string | null;
  aAskSizeText: string | null;
  aBidOrderCount: number | null;
  aAskOrderCount: number | null;
  bBid: number | null;
  bAsk: number | null;
  bBidSize: number | null;
  bAskSize: number | null;
  bBidSizeText: string | null;
  bAskSizeText: string | null;
  bBidOrderCount: number | null;
  bAskOrderCount: number | null;
  aMid: number | null;
  bMid: number | null;
  aToB: number | null;
  bToA: number | null;
  aToBBp: number | null;
  bToABp: number | null;
  midDiff: number | null;
};

export type SpreadResponse = {
  meta: {
    fromMs: number;
    toMs: number;
    bucketSeconds: number;
    granularity: SpreadGranularity;
    requestedPrecision: SpreadRequestedPrecision;
    source: SpreadSource;
    fallbackReason: 'candle_disabled' | 'candle_not_ready' | 'coverage_gap' | null;
    coverage: {
      fromMs: number;
      toMs: number;
      complete: boolean;
    };
    nextCursor: string | null;
    sourceRows: number;
    bookStatePolicy: 'carry_forward_with_expiry';
    maxStaleMs: number;
    targetQuote: string;
    aRate: number;
    bRate: number;
    instrumentA: Instrument;
    instrumentB: Instrument;
  };
  points: SpreadPoint[];
};
