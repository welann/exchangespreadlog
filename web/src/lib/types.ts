export type Instrument = {
  instrumentKey: string;
  venue: string;
  instrumentId: string;
  symbol: string;
  baseAsset: string;
  quoteAsset: string;
  latestRecvMs: number | null;
  status: string;
};

export type Market = {
  baseAsset: string;
  quoteAssets: string[];
  instruments: Instrument[];
};

export type SpreadPoint = {
  tsMs: number;
  aStateTsMs: number;
  bStateTsMs: number;
  aBid: number;
  aAsk: number;
  bBid: number;
  bAsk: number;
  aToB: number;
  bToA: number;
  aToBBp: number;
  bToABp: number;
};

export type LiveSpread = {
  state: 'valid' | 'unavailable';
  reason: string | null;
  targetQuote: string;
  point: SpreadPoint | null;
  legA: Instrument;
  legB: Instrument;
};

export type HistoryResponse = {
  fromMs: number;
  toMs: number;
  resolutionMs: number;
  sourceRows: number;
  targetQuote: string;
  points: SpreadPoint[];
};

export type Health = {
  status: 'ok' | 'degraded';
  clickhouseVersion: string | null;
  walPending: number | null;
  receivedTicks: number;
  acceptedTicks: number;
  rejectedTicks: number;
  venueResets: number;
};
