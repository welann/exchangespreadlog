import type { Instrument } from '$lib/types';
import { ClickHouseError, queryClickHouse, quoteString, tickTable } from './clickhouse';

type DescribeRow = {
  name: string;
  type: string;
};

export type TickSchema = {
  hasCatalogId: boolean;
  hasLegacyVenueMarket: boolean;
  mode: 'catalog_id' | 'legacy_venue_market' | 'hybrid' | 'unsupported';
};

let cachedTickSchema: Promise<TickSchema> | null = null;

export function getTickSchema(): Promise<TickSchema> {
  cachedTickSchema ??= queryClickHouse<DescribeRow>(
    `DESCRIBE TABLE ${tickTable()} FORMAT JSONEachRow`
  ).then((rows) => {
    const columns = new Set(rows.map((row) => row.name));
    const hasCatalogId = columns.has('catalog_id');
    const hasLegacyVenueMarket = columns.has('venue') && columns.has('market_id');
    const mode =
      hasCatalogId && hasLegacyVenueMarket
        ? 'hybrid'
        : hasCatalogId
          ? 'catalog_id'
          : hasLegacyVenueMarket
            ? 'legacy_venue_market'
            : 'unsupported';

    return { hasCatalogId, hasLegacyVenueMarket, mode };
  });

  return cachedTickSchema;
}

export function assertSupportedTickSchema(schema: TickSchema): void {
  if (schema.mode === 'unsupported') {
    throw new ClickHouseError(
      'bbo_ticks must contain either catalog_id or legacy venue + market_id columns',
      500
    );
  }
}

export function tickIdentityWhere(
  schema: TickSchema,
  instrument: Instrument,
  alias: string
): string {
  assertSupportedTickSchema(schema);
  const prefix = `${alias}.`;
  const predicates = [];

  if (schema.hasCatalogId) {
    predicates.push(`${prefix}catalog_id = ${quoteString(instrument.catalogId)}`);
  }

  if (schema.hasLegacyVenueMarket) {
    const legacyGuard = schema.hasCatalogId ? `${prefix}catalog_id = '' AND ` : '';
    predicates.push(
      `(${legacyGuard}${prefix}venue = ${quoteString(instrument.venueInstanceId)} AND ${prefix}market_id = ${quoteString(instrument.instrumentId)})`
    );
  }

  return predicates.length === 1 ? predicates[0] : `(${predicates.join(' OR ')})`;
}

export function usableTickWhere(schema: TickSchema, alias = ''): string {
  assertSupportedTickSchema(schema);
  const prefix = alias ? `${alias}.` : '';
  const predicates = [];
  if (schema.hasCatalogId) predicates.push(`${prefix}catalog_id != ''`);
  if (schema.hasLegacyVenueMarket) {
    const legacyGuard = schema.hasCatalogId ? `${prefix}catalog_id = '' AND ` : '';
    predicates.push(`(${legacyGuard}${prefix}venue != '' AND ${prefix}market_id != '')`);
  }
  return predicates.length === 1 ? predicates[0] : `(${predicates.join(' OR ')})`;
}
