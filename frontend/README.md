# Exchange Spread Log Frontend

SvelteKit dashboard for inspecting cross-venue BBO spread curves from ClickHouse.

## Run locally

```bash
cd frontend
npm install
cp .env.example .env
npm run dev
```

Open `http://localhost:5173`.

## ClickHouse settings

The browser talks only to SvelteKit API routes. ClickHouse credentials stay in the server environment.

The frontend reads ClickHouse settings in this order:

1. Environment variables.
2. `CLICKHOUSE_CONFIG` or `EXCHANGESPREADLOG_CONFIG`, pointing at a TOML config file.
3. A nearby `config.toml` when running from the repository.

Supported environment variables:

- `CLICKHOUSE_URL`
- `CLICKHOUSE_DATABASE` or `CLICKHOUSE_DB`
- `CLICKHOUSE_TABLE`
- `CLICKHOUSE_CATALOG_TABLE`
- `CLICKHOUSE_USERNAME` or `CLICKHOUSE_USER`
- `CLICKHOUSE_PASSWORD` or `CLICKHOUSE_PASS`

Quote conversion rates are configured in the web page under **Quote conversion**. They are stored in browser localStorage and sent with each spread query, so changing rates does not require restarting the server.

For deployment diagnostics, open `/api/health`. It returns the effective ClickHouse URL/database/table, whether a password was provided, row counts, latest tick time, detected tick schema, usable tick rows, and an `apiVersion`. It never returns the password.

The current schema-aware frontend should report:

- `apiVersion: "clickhouse-schema-aware-v2"`
- `tickSchema.mode`: `catalog_id`, `legacy_venue_market`, or `hybrid`
- `stats.usableTickRows`: rows that can be mapped to an instrument and used by spread charts

If `/api/health` does not include `apiVersion` and `tickSchema`, the deployed frontend is still an older build.

## Docker

```bash
docker build -t exchangespreadlog-frontend ./frontend
docker run --rm -p 3000:3000 \
  -e CLICKHOUSE_URL=https://obdata.zeabur.app/ \
  -e CLICKHOUSE_DATABASE=zeabur \
  -e CLICKHOUSE_TABLE=bbo_ticks \
  -e CLICKHOUSE_CATALOG_TABLE=instrument_catalog \
  -e CLICKHOUSE_USERNAME=zeabur \
  -e CLICKHOUSE_PASSWORD=your-clickhouse-password \
  exchangespreadlog-frontend
```

Open `http://localhost:3000`.

You can also mount the collector config and only provide the password environment variable:

```bash
docker run --rm -p 3000:3000 \
  -v "$PWD/config.toml:/app/config.toml:ro" \
  -e CLICKHOUSE_PASSWORD=your-clickhouse-password \
  exchangespreadlog-frontend
```

If you keep values in an env file, pass it explicitly:

```bash
docker run --rm -p 3000:3000 --env-file ./frontend/.env exchangespreadlog-frontend
```
