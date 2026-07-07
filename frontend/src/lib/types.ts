export type QuoteRate = {
  from: string;
  to: string;
  rate: string;
};

export type Instrument = {
  catalogId: string;
  venueInstanceId: string;
  instrumentId: string;
  rawSymbol: string;
  baseAsset: string;
  quoteAsset: string;
  status: string;
  latestRecvMs: number | null;
  tickCount: number;
  label: string;
};

export type Market = {
  baseAsset: string;
  instruments: Instrument[];
};

export type SpreadPoint = {
  tsMs: number;
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
    granularity: 'raw' | 'bucket';
    sourceRows: number;
    targetQuote: string;
    aRate: number;
    bRate: number;
    instrumentA: Instrument;
    instrumentB: Instrument;
  };
  points: SpreadPoint[];
};
