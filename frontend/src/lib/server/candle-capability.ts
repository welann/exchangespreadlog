import type { SpreadGranularity } from '$lib/types';
import { candleRangeIsCovered } from '$lib/spread/candle-coverage';
import {
  clickHouseConfig,
  configuredTable,
  queryClickHouse,
  quoteString
} from './clickhouse';

const SCHEMA_VERSION = 2;
const CACHE_TTL_MS = 60_000;
const CANDLE_SUFFIXES = ['_candle_1s', '_candle_1m', '_candle_5m', '_candle_15m', '_candle_1h'];

type StatusRow = {
  ready: number | string;
  coverageFromMs: number | string | null;
  coverageToMs: number | string | null;
};

type LatestValidCoverageRow = {
  coverageToMs: number | string | null;
};

export type CandleCapability = {
  mode: 'off' | 'auto' | 'force';
  schemaVersion: number;
  ready: boolean;
  reason: 'candle_disabled' | 'candle_not_ready' | null;
  coverageFromMs: number | null;
  coverageToMs: number | null;
  latestValidTable: boolean;
  tables: Record<Exclude<SpreadGranularity, 'raw' | 'bucket'>, boolean>;
};

let cached: { expiresAt: number; value: CandleCapability } | null = null;

export async function getCandleCapability(forceRefresh = false): Promise<CandleCapability> {
  const config = clickHouseConfig();
  if (!forceRefresh && cached && cached.expiresAt > Date.now() && cached.value.mode === config.candleMode) {
    return cached.value;
  }

  const disabled = capabilityBase(config.candleMode);
  if (config.candleMode === 'off') {
    cached = { expiresAt: Date.now() + CACHE_TTL_MS, value: disabled };
    return disabled;
  }

  const expectedNames = CANDLE_SUFFIXES.map((suffix) => `${config.table}${suffix}`);
  const statusName = `${config.table}_candle_status`;
  const latestValidName = `${config.table}_latest_valid`;
  const rows = await queryClickHouse<{ name: string }>(`
SELECT name
FROM system.tables
WHERE database = ${quoteString(config.database)}
  AND name IN (${[...expectedNames, statusName, latestValidName].map(quoteString).join(', ')})
FORMAT JSONEachRow
`);
  const existing = new Set(rows.map((row) => row.name));
  const tables = {
    '1s': existing.has(`${config.table}_candle_1s`),
    '1m': existing.has(`${config.table}_candle_1m`),
    '5m': existing.has(`${config.table}_candle_5m`),
    '15m': existing.has(`${config.table}_candle_15m`),
    '1h': existing.has(`${config.table}_candle_1h`)
  } as const;

  const [statusRows, latestCoverageRows] = await Promise.all([
    existing.has(statusName)
      ? queryClickHouse<StatusRow>(`
SELECT
  argMax(ready, updated_at) AS ready,
  toUnixTimestamp64Milli(argMax(coverage_from, updated_at)) AS coverageFromMs,
  toUnixTimestamp64Milli(argMax(coverage_to, updated_at)) AS coverageToMs
FROM ${configuredTable('_candle_status')}
WHERE schema_version = ${SCHEMA_VERSION}
FORMAT JSONEachRow
`)
      : Promise.resolve([]),
    existing.has(latestValidName)
      ? queryClickHouse<LatestValidCoverageRow>(`
SELECT
  toUnixTimestamp64Milli(max(latest_recv_time)) AS coverageToMs
FROM
(
  SELECT maxMerge(latest_recv_time) AS latest_recv_time
  FROM ${configuredTable('_latest_valid')}
  GROUP BY venue_instance_id, instrument_id
)
FORMAT JSONEachRow
`)
      : Promise.resolve([])
  ]);
  const status = statusRows[0];
  const latestCoverage = nullableNumber(latestCoverageRows[0]?.coverageToMs);
  const statusCoverage = nullableNumber(status?.coverageToMs);

  const latestValidTable = existing.has(latestValidName);
  const allTablesExist = Object.values(tables).every(Boolean) && latestValidTable;
  const ready = allTablesExist && Number(status?.ready ?? 0) === 1;
  const value: CandleCapability = {
    mode: config.candleMode,
    schemaVersion: SCHEMA_VERSION,
    ready,
    reason: ready ? null : 'candle_not_ready',
    coverageFromMs: nullableNumber(status?.coverageFromMs),
    coverageToMs:
      latestCoverage === null
        ? statusCoverage
        : statusCoverage === null
          ? latestCoverage
          : Math.max(statusCoverage, latestCoverage),
    latestValidTable,
    tables
  };
  cached = { expiresAt: Date.now() + CACHE_TTL_MS, value };
  return value;
}

export function candleCoversRange(
  capability: CandleCapability,
  fromMs: number,
  toMs: number,
  nowMs = Date.now(),
  maxStaleMs = clickHouseConfig().maxStaleMs
): boolean {
  return candleRangeIsCovered(capability, fromMs, toMs, nowMs, maxStaleMs);
}

function capabilityBase(mode: CandleCapability['mode']): CandleCapability {
  return {
    mode,
    schemaVersion: SCHEMA_VERSION,
    ready: false,
    reason: mode === 'off' ? 'candle_disabled' : 'candle_not_ready',
    coverageFromMs: null,
    coverageToMs: null,
    latestValidTable: false,
    tables: { '1s': false, '1m': false, '5m': false, '15m': false, '1h': false }
  };
}

function nullableNumber(value: unknown): number | null {
  if (value === null || value === undefined) return null;
  const parsed = Number(value);
  return Number.isFinite(parsed) && parsed > 0 ? parsed : null;
}
