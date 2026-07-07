import type { Instrument, Market } from '$lib/types';
import { catalogTable, queryClickHouse, quoteString, tickTable } from './clickhouse';
import { assertSupportedTickSchema, getTickSchema } from './tick-schema';

type RawInstrument = {
  catalogId: string;
  venueInstanceId: string;
  instrumentId: string;
  rawSymbol: string;
  baseAsset: string;
  quoteAsset: string;
  status: string;
  latestRecvMs: number | string | null;
  tickCount: number | string | null;
};

export async function fetchInstruments(catalogIds?: string[]): Promise<Instrument[]> {
  if (catalogIds?.length === 0) return [];

  const tickSchema = await getTickSchema();
  assertSupportedTickSchema(tickSchema);
  const stats = tickStatsSql(tickSchema);
  const catalogFilter =
    catalogIds && catalogIds.length > 0
      ? `latest.catalog_id IN (${catalogIds.map(quoteString).join(', ')})`
      : "latest.status = 'active'";

  const rows = await queryClickHouse<RawInstrument>(`
SELECT
  latest.catalog_id AS catalogId,
  latest.venue_instance_id AS venueInstanceId,
  latest.instrument_id AS instrumentId,
  latest.raw_symbol AS rawSymbol,
  latest.base_asset AS baseAsset,
  latest.quote_asset AS quoteAsset,
  latest.status AS status,
  ${stats.latestRecvMsSql} AS latestRecvMs,
  ${stats.tickCountSql} AS tickCount
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
${stats.joinsSql}
WHERE ${catalogFilter}
ORDER BY latest.base_asset ASC, latest.venue_instance_id ASC, latest.raw_symbol ASC
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
    latestRecvMs: nullableNumber(row.latestRecvMs),
    tickCount: Number(row.tickCount ?? 0),
    label
  };
}

function nullableNumber(value: unknown): number | null {
  if (value === null || value === undefined) return null;
  const parsed = Number(value);
  return Number.isFinite(parsed) ? parsed : null;
}

function tickStatsSql(schema: Awaited<ReturnType<typeof getTickSchema>>) {
  const joins: string[] = [];
  const latestCandidates: string[] = [];
  const countCandidates: string[] = [];

  if (schema.hasCatalogId) {
    joins.push(`
LEFT JOIN
(
  SELECT
    ticks.catalog_id AS catalog_id,
    max(ticks.recv_time) AS latest_recv_time,
    count() AS tick_count
  FROM ${tickTable()} AS ticks
  WHERE ticks.catalog_id != ''
  GROUP BY ticks.catalog_id
) AS catalog_tick_stats ON latest.catalog_id = catalog_tick_stats.catalog_id`);
    latestCandidates.push('catalog_tick_stats.latest_recv_time');
    countCandidates.push('ifNull(catalog_tick_stats.tick_count, 0)');
  }

  if (schema.hasLegacyVenueMarket) {
    const legacyWhere = schema.hasCatalogId
      ? "ticks.catalog_id = '' AND ticks.venue != '' AND ticks.market_id != ''"
      : "ticks.venue != '' AND ticks.market_id != ''";
    joins.push(`
LEFT JOIN
(
  SELECT
    ticks.venue AS venue_instance_id,
    ticks.market_id AS instrument_id,
    max(ticks.recv_time) AS latest_recv_time,
    count() AS tick_count
  FROM ${tickTable()} AS ticks
  WHERE ${legacyWhere}
  GROUP BY ticks.venue, ticks.market_id
) AS legacy_tick_stats
  ON latest.venue_instance_id = legacy_tick_stats.venue_instance_id
 AND latest.instrument_id = legacy_tick_stats.instrument_id`);
    latestCandidates.push('legacy_tick_stats.latest_recv_time');
    countCandidates.push('ifNull(legacy_tick_stats.tick_count, 0)');
  }

  return {
    joinsSql: joins.join('\n'),
    latestRecvMsSql: `if(isNull(${greatestNullable(latestCandidates)}), NULL, toUnixTimestamp64Milli(${greatestNullable(latestCandidates)}))`,
    tickCountSql: countCandidates.join(' + ')
  };
}

function greatestNullable(values: string[]): string {
  if (values.length === 1) return values[0];
  const [first, second] = values;
  return `multiIf(isNull(${first}), ${second}, isNull(${second}), ${first}, greatest(${first}, ${second}))`;
}
