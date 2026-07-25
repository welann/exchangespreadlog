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
  tickCount: number | string | null;
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

  const stats = includeTickStats
    ? await getTickSchema().then((tickSchema) => {
        assertSupportedTickSchema(tickSchema);
        return tickStatsSql(tickSchema);
      })
    : {
        joinsSql: '',
        latestRecvMsSql: 'NULL',
        tickCountSql: '0'
      };
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
  ${stats.latestRecvMsSql} AS latestRecvMs,
  ${stats.tickCountSql} AS tickCount
FROM
(
  ${latestCatalogSql}
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
  const uniqueInstruments = new Map<string, Instrument>();
  for (const instrument of instruments) {
    if (instrument.latestRecvMs === null || instrument.tickCount <= 0) continue;
    const key = `${instrument.venueInstanceId}\u0000${instrument.instrumentId}`;
    const current = uniqueInstruments.get(key);
    if (
      !current ||
      (instrument.latestRecvMs ?? 0) > (current.latestRecvMs ?? 0) ||
      instrument.tickCount > current.tickCount
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
  const validBooks = validBookWhere(schema, 'ticks');

  if (schema.hasStorageIdentity) {
    joins.push(`
LEFT JOIN
(
  SELECT
    ticks.venue_instance_id AS venue_instance_id,
    ticks.instrument_id AS instrument_id,
    max(ticks.recv_time) AS latest_recv_time,
    count() AS tick_count
  FROM ${tickTable()} AS ticks
  WHERE ticks.venue_instance_id != '' AND ticks.instrument_id != ''
    AND ${validBooks}
    AND ticks.recv_time >= now() - INTERVAL ${TICK_STATS_WINDOW_DAYS} DAY
  GROUP BY ticks.venue_instance_id, ticks.instrument_id
) AS storage_tick_stats
  ON latest.venue_instance_id = storage_tick_stats.venue_instance_id
 AND latest.instrument_id = storage_tick_stats.instrument_id`);
    latestCandidates.push('storage_tick_stats.latest_recv_time');
    countCandidates.push('ifNull(storage_tick_stats.tick_count, 0)');
  }

  if (!schema.hasStorageIdentity && schema.hasCatalogId) {
    joins.push(`
LEFT JOIN
(
  SELECT
    ticks.catalog_id AS catalog_id,
    max(ticks.recv_time) AS latest_recv_time,
    count() AS tick_count
  FROM ${tickTable()} AS ticks
  WHERE ticks.catalog_id != ''
    AND ${validBooks}
    AND ticks.recv_time >= now() - INTERVAL ${TICK_STATS_WINDOW_DAYS} DAY
  GROUP BY ticks.catalog_id
) AS catalog_tick_stats ON latest.catalog_id = catalog_tick_stats.catalog_id`);
    latestCandidates.push('catalog_tick_stats.latest_recv_time');
    countCandidates.push('ifNull(catalog_tick_stats.tick_count, 0)');
  }

  if (!schema.hasStorageIdentity && schema.hasLegacyVenueMarket) {
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
    AND ${validBooks}
    AND ticks.recv_time >= now() - INTERVAL ${TICK_STATS_WINDOW_DAYS} DAY
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
