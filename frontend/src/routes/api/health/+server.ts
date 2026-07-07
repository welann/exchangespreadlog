import { json, type RequestHandler } from '@sveltejs/kit';
import {
  ClickHouseError,
  catalogTable,
  clickHouseConfigSummary,
  queryClickHouse,
  tickTable
} from '$lib/server/clickhouse';
import { getTickSchema, usableTickWhere } from '$lib/server/tick-schema';

type HealthRow = {
  tickRows: number | string;
  catalogRows: number | string;
  latestRecvMs: number | string | null;
  usableTickRows: number | string;
  latestUsableRecvMs: number | string | null;
};

export const GET: RequestHandler = async () => {
  try {
    const clickhouse = clickHouseConfigSummary();
    const tickSchema = await getTickSchema();
    const [row] = await queryClickHouse<HealthRow>(`
SELECT
  (SELECT count() FROM ${tickTable()}) AS tickRows,
  (SELECT count() FROM ${catalogTable()}) AS catalogRows,
  (SELECT if(isNull(max(all_ticks.recv_time)), NULL, toUnixTimestamp64Milli(max(all_ticks.recv_time))) FROM ${tickTable()} AS all_ticks) AS latestRecvMs,
  (SELECT count() FROM ${tickTable()} AS usable_ticks WHERE ${usableTickWhere(tickSchema, 'usable_ticks')}) AS usableTickRows,
  (SELECT if(isNull(max(usable_ticks.recv_time)), NULL, toUnixTimestamp64Milli(max(usable_ticks.recv_time))) FROM ${tickTable()} AS usable_ticks WHERE ${usableTickWhere(tickSchema, 'usable_ticks')}) AS latestUsableRecvMs
FORMAT JSONEachRow
`);

    return json({
      ok: true,
      apiVersion: 'clickhouse-schema-aware-v2',
      clickhouse,
      tickSchema,
      stats: {
        tickRows: Number(row?.tickRows ?? 0),
        catalogRows: Number(row?.catalogRows ?? 0),
        latestRecvMs: nullableNumber(row?.latestRecvMs),
        latestRecvTime: row?.latestRecvMs ? new Date(Number(row.latestRecvMs)).toISOString() : null,
        usableTickRows: Number(row?.usableTickRows ?? 0),
        latestUsableRecvMs: nullableNumber(row?.latestUsableRecvMs),
        latestUsableRecvTime: row?.latestUsableRecvMs
          ? new Date(Number(row.latestUsableRecvMs)).toISOString()
          : null
      }
    });
  } catch (error) {
    return apiError(error);
  }
};

function nullableNumber(value: unknown): number | null {
  if (value === null || value === undefined) return null;
  const parsed = Number(value);
  return Number.isFinite(parsed) ? parsed : null;
}

function apiError(error: unknown): Response {
  const status = error instanceof ClickHouseError ? error.status : 500;
  const message = error instanceof Error ? error.message : 'Unknown server error';
  let clickhouse = null;
  try {
    clickhouse = clickHouseConfigSummary();
  } catch {
    // Keep the original error as the useful failure.
  }
  return json({ ok: false, apiVersion: 'clickhouse-schema-aware-v2', error: message, clickhouse }, { status });
}
