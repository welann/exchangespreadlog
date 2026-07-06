import type { Instrument, Market } from '$lib/types';
import { catalogTable, queryClickHouse, quoteString } from './clickhouse';

type RawInstrument = {
  catalogId: string;
  venueInstanceId: string;
  instrumentId: string;
  rawSymbol: string;
  baseAsset: string;
  quoteAsset: string;
  status: string;
};

export async function fetchInstruments(catalogIds?: string[]): Promise<Instrument[]> {
  if (catalogIds?.length === 0) return [];

  const catalogFilter =
    catalogIds && catalogIds.length > 0
      ? `catalog_id IN (${catalogIds.map(quoteString).join(', ')})`
      : "status = 'active'";

  const rows = await queryClickHouse<RawInstrument>(`
SELECT
  catalog_id AS catalogId,
  venue_instance_id AS venueInstanceId,
  instrument_id AS instrumentId,
  raw_symbol AS rawSymbol,
  base_asset AS baseAsset,
  quote_asset AS quoteAsset,
  status
FROM
(
  SELECT
    catalog_id,
    argMax(venue_instance_id, inserted_time) AS venue_instance_id,
    argMax(instrument_id, inserted_time) AS instrument_id,
    argMax(raw_symbol, inserted_time) AS raw_symbol,
    argMax(base_asset, inserted_time) AS base_asset,
    argMax(quote_asset, inserted_time) AS quote_asset,
    argMax(status, inserted_time) AS status
  FROM ${catalogTable()}
  GROUP BY catalog_id
) AS latest
WHERE ${catalogFilter}
ORDER BY base_asset ASC, venue_instance_id ASC, raw_symbol ASC
FORMAT JSONEachRow
`);

  return rows.map(toInstrument);
}

export function groupMarkets(instruments: Instrument[]): Market[] {
  const markets = new Map<string, Instrument[]>();
  for (const instrument of instruments) {
    const rows = markets.get(instrument.baseAsset) ?? [];
    rows.push(instrument);
    markets.set(instrument.baseAsset, rows);
  }

  return [...markets.entries()]
    .map(([baseAsset, rows]) => ({
      baseAsset,
      instruments: rows.sort((a, b) => a.label.localeCompare(b.label))
    }))
    .filter((market) => market.instruments.length >= 2)
    .sort((a, b) => a.baseAsset.localeCompare(b.baseAsset));
}

function toInstrument(row: RawInstrument): Instrument {
  const rawSymbol = row.rawSymbol || row.instrumentId;
  const label = `${row.venueInstanceId} ${rawSymbol}/${row.quoteAsset}`;
  return {
    catalogId: row.catalogId,
    venueInstanceId: row.venueInstanceId,
    instrumentId: row.instrumentId,
    rawSymbol,
    baseAsset: row.baseAsset,
    quoteAsset: row.quoteAsset,
    status: row.status,
    label
  };
}
