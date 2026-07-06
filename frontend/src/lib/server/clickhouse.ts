import { env } from '$env/dynamic/private';

type ClickHouseConfig = {
  url: string;
  database: string;
  table: string;
  catalogTable: string;
  username: string;
  password: string;
};

export class ClickHouseError extends Error {
  constructor(
    message: string,
    readonly status = 500
  ) {
    super(message);
    this.name = 'ClickHouseError';
  }
}

export function clickHouseConfig(): ClickHouseConfig {
  const url = (env.CLICKHOUSE_URL ?? 'http://localhost:8123').trim().replace(/\/+$/, '');
  if (!url) {
    throw new ClickHouseError('CLICKHOUSE_URL is empty');
  }

  const config = {
    url,
    database: env.CLICKHOUSE_DATABASE ?? 'default',
    table: env.CLICKHOUSE_TABLE ?? 'bbo_ticks',
    catalogTable: env.CLICKHOUSE_CATALOG_TABLE ?? 'instrument_catalog',
    username: env.CLICKHOUSE_USERNAME ?? '',
    password: env.CLICKHOUSE_PASSWORD ?? ''
  };

  validateIdentifier('CLICKHOUSE_DATABASE', config.database);
  validateIdentifier('CLICKHOUSE_TABLE', config.table);
  validateIdentifier('CLICKHOUSE_CATALOG_TABLE', config.catalogTable);

  return config;
}

export function tickTable(): string {
  return quoteIdentifier(clickHouseConfig().table);
}

export function catalogTable(): string {
  return quoteIdentifier(clickHouseConfig().catalogTable);
}

export async function queryClickHouse<T>(sql: string): Promise<T[]> {
  const config = clickHouseConfig();
  const url = new URL(config.url);
  url.searchParams.set('database', config.database);

  const headers = new Headers({
    'content-type': 'text/plain; charset=utf-8'
  });

  if (config.username) {
    const token = Buffer.from(`${config.username}:${config.password}`).toString('base64');
    headers.set('authorization', `Basic ${token}`);
  }

  const response = await fetch(url, {
    method: 'POST',
    headers,
    body: sql
  });

  const body = await response.text();
  if (!response.ok) {
    throw new ClickHouseError(
      `ClickHouse HTTP ${response.status}: ${body.slice(0, 800)}`,
      response.status
    );
  }

  return parseJsonEachRow<T>(body);
}

export function quoteString(value: string): string {
  return `'${value.replace(/\\/g, '\\\\').replace(/'/g, "\\'")}'`;
}

export function numericLiteral(value: number): string {
  if (!Number.isFinite(value)) {
    throw new ClickHouseError('Invalid numeric literal', 400);
  }
  return String(value);
}

function parseJsonEachRow<T>(body: string): T[] {
  return body
    .split('\n')
    .map((line) => line.trim())
    .filter(Boolean)
    .map((line) => JSON.parse(line) as T);
}

function validateIdentifier(label: string, value: string): void {
  if (!/^[A-Za-z_][A-Za-z0-9_]*$/.test(value)) {
    throw new ClickHouseError(`${label} must be a simple ClickHouse identifier`, 500);
  }
}

function quoteIdentifier(value: string): string {
  validateIdentifier('identifier', value);
  return `\`${value}\``;
}
