import type { Instrument, Market } from '$lib/types';
import { validBookWhere } from './book-filter';
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
};

const INSTRUMENT_CACHE_TTL_MS = 300_000;
const TICK_STATS_WINDOW_DAYS = 7;
const MAX_METADATA_CACHE_ENTRIES = 512;

let cachedInstruments: { expiresAt: number; value: Instrument[] } | null = null;
let instrumentsInFlight: Promise<Instrument[]> | null = null;
const metadataByCatalogId = new Map<string, Instrument>();

export async function fetchInstruments(catalogIds?: string[]): Promise<Instrument[]> {
  if (catalogIds) return queryInstruments(catalogIds, true);
  if (cachedInstruments && cachedInstruments.expiresAt > Date.now()) {
    return cachedInstruments.value;
  }
  if (instrumentsInFlight) return instrumentsInFlight;

  instrumentsInFlight = queryInstruments(undefined, true)
    .then((instruments) => {
      rememberInstrumentMetadata(instruments);
      cachedInstruments = {
        expiresAt: Date.now() + INSTRUMENT_CACHE_TTL_MS,
        value: instruments
      };
      return instruments;
    })
    .finally(() => {
      instrumentsInFlight = null;
    });
  return instrumentsInFlight;
}

export async function fetchInstrumentMetadata(catalogIds: string[]): Promise<Instrument[]> {
  const uniqueIds = [...new Set(catalogIds)];
  const cached = uniqueIds
    .map((catalogId) => metadataByCatalogId.get(catalogId))
    .filter((instrument): instrument is Instrument => instrument !== undefined);
  const cachedIds = new Set(cached.map((instrument) => instrument.catalogId));
  const missingIds = uniqueIds.filter((catalogId) => !cachedIds.has(catalogId));
  if (missingIds.length === 0) return cached;

  const fetched = await queryInstruments(missingIds, false);
  rememberInstrumentMetadata(fetched);
  const byCatalogId = new Map(
    [...cached, ...fetched].map((instrument) => [instrument.catalogId, instrument])
  );
  return uniqueIds
    .map((catalogId) => byCatalogId.get(catalogId))
    .filter((instrument): instrument is Instrument => instrument !== undefined);
}

async function queryInstruments(
  catalogIds: string[] | undefined,
  includeTickStats: boolean
): Promise<Instrument[]> {
  if (catalogIds?.length === 0) return [];

  const joinSql = includeTickStats
    ? await getTickSchema().then((tickSchema) => {
        assertSupportedTickSchema(tickSchema);
        return buildTickStatsJoin(tickSchema);
      })
    : '';
  const latestRecvMsSql = joinSql
    ? 'toUnixTimestamp64Milli(tick_stats.latest_recv_time)'
    : 'NULL';
  const catalogFilter =
    catalogIds && catalogIds.length > 0
      ? `latest.catalog_id IN (${catalogIds.map(quoteString).join(', ')})`
      : "latest.status = 'active'";
  const latestCatalogSql =
    catalogIds && catalogIds.length > 0
      ? selectedCatalogSql(catalogIds)
      : latestStorageIdentityCatalogSql();

  const rows = await queryClickHouse<RawInstrument>(`
SELECT
  latest.catalog_id AS catalogId,
  latest.venue_instance_id AS venueInstanceId,
  latest.instrument_id AS instrumentId,
  latest.raw_symbol AS rawSymbol,
  latest.base_asset AS baseAsset,
  latest.quote_asset AS quoteAsset,
  latest.status AS status,
  ${latestRecvMsSql} AS latestRecvMs
FROM
(
  ${latestCatalogSql}
) AS latest
${joinSql}
WHERE ${catalogFilter}
ORDER BY latest.base_asset ASC, latest.venue_instance_id ASC, latest.raw_symbol ASC
FORMAT JSONEachRow
`);

  return rows.map(toInstrument);
}

export function groupMarkets(instruments: Instrument[]): Market[] {
  const markets = new Map<string, Instrument[]>();
  const uniqueInstruments = new Map<string, Instrument>();
  for (const instrument of instruments) {
    if (instrument.latestRecvMs === null) continue;
    const key = `${instrument.venueInstanceId}\u0000${instrument.instrumentId}`;
    const current = uniqueInstruments.get(key);
    if (
      !current ||
      (instrument.latestRecvMs ?? 0) > (current.latestRecvMs ?? 0)
    ) {
      uniqueInstruments.set(key, instrument);
    }
  }

  for (const instrument of uniqueInstruments.values()) {
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

function selectedCatalogSql(catalogIds: string[]): string {
  return `
  SELECT
    catalog_id,
    argMax(venue_instance_id, inserted_time) AS venue_instance_id,
    argMax(instrument_id, inserted_time) AS instrument_id,
    argMax(raw_symbol, inserted_time) AS raw_symbol,
    argMax(base_asset, inserted_time) AS base_asset,
    argMax(quote_asset, inserted_time) AS quote_asset,
    argMax(status, inserted_time) AS status
  FROM ${catalogTable()}
  WHERE catalog_id IN (${catalogIds.map(quoteString).join(', ')})
  GROUP BY catalog_id`;
}

function latestStorageIdentityCatalogSql(): string {
  return `
  SELECT
    argMax(catalog_id, inserted_time) AS catalog_id,
    venue_instance_id,
    instrument_id,
    argMax(raw_symbol, inserted_time) AS raw_symbol,
    argMax(base_asset, inserted_time) AS base_asset,
    argMax(quote_asset, inserted_time) AS quote_asset,
    argMax(status, inserted_time) AS status
  FROM ${catalogTable()}
  GROUP BY venue_instance_id, instrument_id`;
}

function rememberInstrumentMetadata(instruments: Instrument[]): void {
  for (const instrument of instruments) {
    metadataByCatalogId.delete(instrument.catalogId);
    metadataByCatalogId.set(instrument.catalogId, instrument);
  }
  while (metadataByCatalogId.size > MAX_METADATA_CACHE_ENTRIES) {
    const oldest = metadataByCatalogId.keys().next().value;
    if (typeof oldest !== 'string') break;
    metadataByCatalogId.delete(oldest);
  }
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
    label
  };
}

function nullableNumber(value: unknown): number | null {
  if (value === null || value === undefined) return null;
  const parsed = Number(value);
  return Number.isFinite(parsed) ? parsed : null;
}

function buildTickStatsJoin(schema: Awaited<ReturnType<typeof getTickSchema>>): string {
  const validBooks = validBookWhere(schema, 'ticks');

  let joinOnClause: string;
  if (schema.hasStorageIdentity) {
    joinOnClause = `ON latest.venue_instance_id = tick_stats.venue_instance_id AND latest.instrument_id = tick_stats.instrument_id`;
  } else if (schema.hasCatalogId) {
    joinOnClause = `ON latest.catalog_id = tick_stats.catalog_id`;
  } else {
    joinOnClause = `ON latest.venue_instance_id = tick_stats.legacy_venue AND latest.instrument_id = tick_stats.legacy_market_id`;
  }

  return `
LEFT JOIN (
  SELECT
    venue_instance_id,
    instrument_id,
    argMax(catalog_id, recv_time) AS catalog_id,
    argMax(venue, recv_time) AS legacy_venue,
    argMax(market_id, recv_time) AS legacy_market_id,
    max(recv_time) AS latest_recv_time
  FROM ${tickTable()} AS ticks
  WHERE venue_instance_id != '' AND instrument_id != ''
    AND ${validBooks}
    AND ticks.recv_time >= now() - INTERVAL ${TICK_STATS_WINDOW_DAYS} DAY
  GROUP BY venue_instance_id, instrument_id
) AS tick_stats
${joinOnClause}`;
}
