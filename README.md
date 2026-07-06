# Exchange Spread Log

Exchange Spread Log is a Rust collector for top-of-book/BBO data from perpetual DEX venues. It connects to exchange WebSocket feeds, normalizes best bid/ask updates, calculates spread and mid price, deduplicates unchanged ticks, shows the latest state in a terminal UI, and optionally persists ticks as JSONL and/or ClickHouse rows.

Currently supported venues:

- `hyperliquid`
- `lighter`
- `risex`
- `01`

By default, Hyperliquid, Lighter, RiseX, and 01 are enabled in `config.example.toml`.

## Project Structure

```text
.
|-- Cargo.toml              # Rust package metadata and dependencies
|-- Cargo.lock              # Locked dependency versions
|-- config.example.toml     # Example runtime configuration
`-- src
    |-- main.rs             # CLI entrypoint
    |-- lib.rs              # Library module exports
    |-- app/runner.rs       # Application orchestration and shutdown handling
    |-- config/             # TOML config model and defaults
    |-- domain/             # BBO tick, market, venue, fixed decimal, quality models
    |-- exchange/           # Exchange adapters and message parsers
    |   |-- hyperliquid/
    |   |-- lighter/
    |   |-- risex/
    |   `-- zero_one/
    |-- ingest/             # WebSocket connection, retry/backoff, time helpers
    |-- pipeline/           # Normalize, dedupe, update state, write sinks
    |-- state.rs            # Shared latest-BBO state for the TUI
    |-- storage/            # JSONL, ClickHouse, fan-out, and no-op storage sinks
    |-- telemetry/          # tracing/logging initialization
    `-- tui.rs              # Ratatui terminal interface
```

## Requirements

- Rust toolchain with Cargo and Rust 2024 edition support
- Network access to the configured WebSocket endpoints

Check the local toolchain:

```bash
cargo --version
```

## Configuration

Create a local config from the example:

```bash
cp config.example.toml config.toml
```

You can also print the built-in default config:

```bash
cargo run -- --print-default-config
```

Important config sections:

- `pipeline.channel_capacity`: internal tick channel size.
- `pipeline.stale_after_ms`: stale threshold reserved in the config model.
- `storage.mode`: storage target. Supported values are `none`, `jsonl`, `clickhouse`, and `both`.
- `storage.jsonl_dir`: base output directory, default `data/bbo`.
- `storage.clickhouse.url`: ClickHouse HTTP endpoint.
- `storage.clickhouse.database`: ClickHouse database name.
- `storage.clickhouse.table`: ClickHouse table name. The collector creates it when `create_table = true`.
- `storage.clickhouse.username`: ClickHouse HTTP username.
- `storage.clickhouse.password_env`: environment variable holding the ClickHouse password.
- `storage.clickhouse.batch_size`: number of rows buffered before each HTTP insert.
- `tui.enabled`: enable or disable the terminal UI.
- `tui.refresh_ms`: terminal UI refresh interval.
- `venues`: exchange WebSocket settings.

Example ClickHouse storage config for the Zeabur service:

```toml
[storage]
mode = "clickhouse"
jsonl_dir = "data/bbo"

[storage.clickhouse]
url = "https://obdata.zeabur.app/"
database = "zeabur"
table = "bbo_ticks"
username = "zeabur"
password_env = "CLICKHOUSE_PASSWORD"
create_table = true
batch_size = 100
```

Set the password before running:

```bash
export CLICKHOUSE_PASSWORD='your-clickhouse-password'
```

Example venue entries:

```toml
[[venues]]
name = "hyperliquid"
enabled = true
url = "wss://api.hyperliquid.xyz/ws"
markets = ["BTC", "ETH", "SOL"]
channel = "bbo"

[[venues]]
name = "lighter"
enabled = false
url = "wss://mainnet.zklighter.elliot.ai/stream"
markets = ["0", "1", "2"]
channel = "ticker"

[[venues]]
name = "risex"
enabled = true
url = "wss://ws.rise.trade/ws"
markets = ["1:BTC", "2:ETH", "4:SOL"]
channel = "orderbook"

[[venues]]
name = "01"
enabled = true
url = "wss://zo-mainnet.n1.xyz"
markets = ["0:BTC:BTCUSD", "1:ETH:ETHUSD", "2:SOL:SOLUSD"]
channel = "deltas"
```

For Hyperliquid, market values are symbols such as `BTC` or `ETH`. For Lighter, market values are market IDs such as `0` or `1`. For RiseX, market values are numeric mainnet market IDs; use `id:symbol` such as `1:BTC` to align the TUI market label with other venues. For 01, use `id:label:feed_symbol`, for example `0:BTC:BTCUSD`; the adapter fetches `GET /market/{id}/orderbook` for the snapshot and subscribes to `deltas@{feed_symbol}` for updates.

## Running

Run with the default `config.toml` path:

```bash
cargo run
```

Run with an explicit config:

```bash
cargo run -- --config config.toml
```

The `--storage` flag overrides `[storage].mode` for that run. Use these commands for the common modes:

1. TUI only, no local or ClickHouse writes:

```bash
cargo run -- --config config.toml --storage none
```

2. TUI plus local JSONL storage:

```bash
cargo run -- --config config.toml --storage jsonl
```

3. TUI plus ClickHouse storage:

```bash
CLICKHOUSE_PASSWORD='your-clickhouse-password' cargo run -- --config config.toml --storage clickhouse
```

4. ClickHouse only, no TUI and no local JSONL storage:

```bash
CLICKHOUSE_PASSWORD='your-clickhouse-password' cargo run -- --config config.toml --storage clickhouse --no-tui
```

If `config.toml` contains `storage.clickhouse.password`, you can omit the `CLICKHOUSE_PASSWORD=...` prefix. Prefer the environment variable for shared or committed configs.

Set log verbosity with `RUST_LOG`:

```bash
RUST_LOG=info cargo run -- --config config.toml --no-tui
RUST_LOG=debug,exchangespreadlog=trace cargo run -- --config config.toml
```

Stop the collector with `Ctrl-C`. When the TUI is enabled, `q` or `Esc` also exits.

## TUI Controls

- `q` / `Esc`: quit.
- `Tab`: switch focus between BBO and spread panels.
- `Left` / `Right`: switch selected market.
- `Up` / `Down`: switch selected venue.
- `1` / `2`: choose the first or second venue leg in the spread panel.

## Output

When `storage.mode = "jsonl"` or `storage.mode = "both"`, ticks are written under:

```text
data/bbo/YYYY-MM-DD/<venue>.jsonl
```

Each line is one normalized `BboTick` JSON object. Common fields include:

- `venue`: exchange name.
- `market`: market ID and optional display symbol.
- `recv_ts_ns`: local receive timestamp in nanoseconds.
- `exchange_ts_ms`: exchange timestamp when provided by the feed.
- `sequence`: feed sequence/nonce when provided.
- `bid` / `ask`: best price, size, and optional order count.
- `spread`: `ask.price - bid.price`.
- `mid`: midpoint between bid and ask.
- `source`: source feed type such as `bbo`, `ticker`, or `l2_book`.
- `quality`: quality flags, including `inconsistent` for negative spread.

When `storage.mode = "clickhouse"` or `storage.mode = "both"`, ticks are inserted through the ClickHouse HTTP interface into `storage.clickhouse.table`. The table includes:

- identifiers and timestamps: `venue`, `market_id`, `market_symbol`, `recv_ts_ns`, `recv_time`, `exchange_ts_ms`, `sequence`, `source`.
- bid/ask values as both Float64 and exact text fields, for example `bid_price` and `bid_price_text`.
- derived `spread` and `mid` values as both Float64 and exact text fields.
- quality flags: `quality_gap`, `quality_stale`, `quality_inconsistent`, `quality_note`.

## Build and Test

Run tests:

```bash
cargo test
```

Build a release binary:

```bash
cargo build --release
```

Run the release binary:

```bash
./target/release/exchangespreadlog --config config.toml
```

Show CLI help:

```bash
cargo run -- --help
```

Current CLI options:

```text
Usage: exchangespreadlog [OPTIONS]

Options:
  -c, --config <CONFIG>       [default: config.toml]
      --no-tui
      --storage <MODE>        Override storage mode: none, jsonl, clickhouse, or both
      --print-default-config
  -h, --help                  Print help
  -V, --version               Print version
```
