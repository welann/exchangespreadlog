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

The browser talks only to SvelteKit API routes. ClickHouse credentials stay in the server environment:

- `CLICKHOUSE_URL`
- `CLICKHOUSE_DATABASE`
- `CLICKHOUSE_TABLE`
- `CLICKHOUSE_CATALOG_TABLE`
- `CLICKHOUSE_USERNAME`
- `CLICKHOUSE_PASSWORD`

Quote conversion rates are configured in the web page under **Quote conversion**. They are stored in browser localStorage and sent with each spread query, so changing rates does not require restarting the server.

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
