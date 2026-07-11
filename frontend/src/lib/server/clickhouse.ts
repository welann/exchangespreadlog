import { env } from '$env/dynamic/private';
import { existsSync, readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { Agent, type Dispatcher, fetch as undiciFetch } from 'undici';

type ClickHouseConfig = {
  url: string;
  database: string;
  table: string;
  catalogTable: string;
  username: string;
  password: string;
  acceptInvalidCerts: boolean;
  source: string;
};

type FileClickHouseConfig = {
  url?: string;
  database?: string;
  table?: string;
  catalogTable?: string;
  username?: string;
  password?: string;
  passwordEnv?: string;
  acceptInvalidCerts?: boolean;
  source: string;
};

type ClickHouseQueryOptions = {
  maxThreads?: number;
};

let insecureDispatcher: Dispatcher | null = null;

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
  const fileConfig = readClickHouseConfigFile();
  const passwordEnvName =
    envValue('CLICKHOUSE_PASSWORD_ENV')?.trim() || fileConfig?.passwordEnv?.trim() || '';
  const passwordFromNamedEnv = passwordEnvName ? envValue(passwordEnvName) : undefined;

  const url = (envValue('CLICKHOUSE_URL') ?? fileConfig?.url ?? 'http://clickhouse.zeabur.internal:8123')
    .trim()
    .replace(/\/+$/, '');
  if (!url) {
    throw new ClickHouseError('CLICKHOUSE_URL is empty');
  }

  const config = {
    url,
    database:
      envValue('CLICKHOUSE_DATABASE') ??
      envValue('CLICKHOUSE_DB') ??
      fileConfig?.database ??
      'default',
    table: envValue('CLICKHOUSE_TABLE') ?? fileConfig?.table ?? 'bbo_ticks',
    catalogTable:
      envValue('CLICKHOUSE_CATALOG_TABLE') ?? fileConfig?.catalogTable ?? 'instrument_catalog',
    username:
      envValue('CLICKHOUSE_USERNAME') ??
      envValue('CLICKHOUSE_USER') ??
      envValue('CLICKHOUSE_HTTP_USERNAME') ??
      envValue('CLICKHOUSE_HTTP_USER') ??
      fileConfig?.username ??
      '',
    password:
      envValue('CLICKHOUSE_PASSWORD') ??
      envValue('CLICKHOUSE_PASS') ??
      envValue('CLICKHOUSE_HTTP_PASSWORD') ??
      passwordFromNamedEnv ??
      fileConfig?.password ??
      '',
    acceptInvalidCerts:
      parseBooleanSetting(
        'CLICKHOUSE_ACCEPT_INVALID_CERTS',
        envValue('CLICKHOUSE_ACCEPT_INVALID_CERTS')
      ) ??
      fileConfig?.acceptInvalidCerts ??
      false,
    source: fileConfig?.source ?? 'environment/defaults'
  };

  validateIdentifier('CLICKHOUSE_DATABASE', config.database);
  validateIdentifier('CLICKHOUSE_TABLE', config.table);
  validateIdentifier('CLICKHOUSE_CATALOG_TABLE', config.catalogTable);

  if (config.username && !config.password && passwordEnvName) {
    const passwordNames = [...new Set([passwordEnvName, 'CLICKHOUSE_PASSWORD'])].join(' or ');
    throw new ClickHouseError(
      `ClickHouse password is missing. Set ${passwordNames}; ${config.source} declares password_env="${passwordEnvName}".`,
      500
    );
  }

  return config;
}

export function clickHouseConfigSummary() {
  const config = clickHouseConfig();
  return {
    url: config.url,
    database: config.database,
    table: config.table,
    catalogTable: config.catalogTable,
    username: config.username || null,
    hasPassword: Boolean(config.password),
    acceptInvalidCerts: config.acceptInvalidCerts,
    source: config.source
  };
}

export function tickTable(): string {
  return quoteIdentifier(clickHouseConfig().table);
}

export function catalogTable(): string {
  return quoteIdentifier(clickHouseConfig().catalogTable);
}

export async function queryClickHouse<T>(
  sql: string,
  options: ClickHouseQueryOptions = {}
): Promise<T[]> {
  const config = clickHouseConfig();
  const url = new URL(config.url);
  url.searchParams.set('database', config.database);
  if (options.maxThreads !== undefined) {
    url.searchParams.set('max_threads', String(options.maxThreads));
  }

  const headers: Record<string, string> = {
    'content-type': 'text/plain; charset=utf-8'
  };

  if (config.username) {
    const token = Buffer.from(`${config.username}:${config.password}`).toString('base64');
    headers.authorization = `Basic ${token}`;
  }

  let response: Awaited<ReturnType<typeof undiciFetch>>;
  try {
    let dispatcher: Dispatcher | undefined;
    if (config.acceptInvalidCerts) {
      insecureDispatcher ??= new Agent({ connect: { rejectUnauthorized: false } });
      dispatcher = insecureDispatcher;
    }
    response = await undiciFetch(url, {
      method: 'POST',
      headers,
      body: sql,
      dispatcher
    });
  } catch (error) {
    const message = error instanceof Error ? error.message : 'unknown network error';
    throw new ClickHouseError(`Cannot reach ClickHouse at ${config.url}: ${message}`, 502);
  }

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

function readClickHouseConfigFile(): FileClickHouseConfig | null {
  for (const candidate of configCandidates()) {
    if (!existsSync(candidate)) continue;
    const raw = readFileSync(candidate, 'utf8');
    const parsed = parseStorageClickHouse(raw);
    if (Object.keys(parsed).length === 0) continue;
    return {
      ...parsed,
      source: candidate
    };
  }
  return null;
}

function configCandidates(): string[] {
  const explicit = envValue('CLICKHOUSE_CONFIG') || envValue('EXCHANGESPREADLOG_CONFIG');
  if (explicit) return [resolve(process.cwd(), explicit)];
  return [
    resolve(process.cwd(), 'config.toml'),
    resolve(process.cwd(), '../config.toml'),
    resolve(process.cwd(), '../../config.toml'),
    '/app/config.toml'
  ];
}

function parseStorageClickHouse(raw: string): Omit<FileClickHouseConfig, 'source'> {
  const parsed: Omit<FileClickHouseConfig, 'source'> = {};
  let section = '';

  for (const line of raw.split('\n')) {
    const trimmed = stripTomlComment(line).trim();
    if (!trimmed) continue;

    const sectionMatch = trimmed.match(/^\[([^\]]+)\]$/);
    if (sectionMatch) {
      section = sectionMatch[1].trim();
      continue;
    }

    if (section !== 'storage.clickhouse') continue;
    const keyValue = trimmed.match(/^([A-Za-z0-9_]+)\s*=\s*(.+)$/);
    if (!keyValue) continue;

    const [, key, rawValue] = keyValue;
    if (key === 'accept_invalid_certs') {
      const value = parseTomlBoolean(rawValue.trim());
      if (value !== null) parsed.acceptInvalidCerts = value;
      continue;
    }

    const value = parseTomlString(rawValue.trim());
    if (typeof value !== 'string') continue;

    if (key === 'url') parsed.url = value;
    if (key === 'database') parsed.database = value;
    if (key === 'table') parsed.table = value;
    if (key === 'catalog_table') parsed.catalogTable = value;
    if (key === 'username') parsed.username = value;
    if (key === 'password') parsed.password = value;
    if (key === 'password_env') parsed.passwordEnv = value;
  }

  return parsed;
}

function stripTomlComment(line: string): string {
  let quoted = false;
  let escaped = false;
  for (let index = 0; index < line.length; index += 1) {
    const char = line[index];
    if (escaped) {
      escaped = false;
      continue;
    }
    if (char === '\\') {
      escaped = true;
      continue;
    }
    if (char === '"') {
      quoted = !quoted;
      continue;
    }
    if (char === '#' && !quoted) {
      return line.slice(0, index);
    }
  }
  return line;
}

function parseTomlString(value: string): string | null {
  if (value.startsWith('"') && value.endsWith('"')) {
    try {
      return JSON.parse(value) as string;
    } catch {
      return value.slice(1, -1);
    }
  }
  return value || null;
}

function parseTomlBoolean(value: string): boolean | null {
  if (value === 'true') return true;
  if (value === 'false') return false;
  return null;
}

function parseBooleanSetting(label: string, value: string | undefined): boolean | undefined {
  if (value === undefined || value.trim() === '') return undefined;
  const normalized = value.trim().toLowerCase();
  if (['1', 'true', 'yes', 'on'].includes(normalized)) return true;
  if (['0', 'false', 'no', 'off'].includes(normalized)) return false;
  throw new ClickHouseError(`${label} must be true or false`, 500);
}

function envValue(name: string): string | undefined {
  return env[name] ?? process.env[name];
}
