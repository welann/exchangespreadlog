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
    |-- domain/             # BBO tick, market catalog, fixed decimal, quality models
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
- `pipeline.stale_after_ms`: marks ticks stale when exchange timestamps lag local receive time by more than this threshold. Set to `0` to disable stale marking.
- `storage.mode`: storage target. Supported values are `none`, `jsonl`, `clickhouse`, and `both`.
- `storage.jsonl_dir`: base output directory, default `data/bbo`.
- `storage.clickhouse.url`: ClickHouse HTTP endpoint.
- `storage.clickhouse.database`: ClickHouse database name.
- `storage.clickhouse.table`: raw BBO tick table name. The collector creates it when `create_table = true`.
- `storage.clickhouse.catalog_table`: instrument catalog table name.
- `storage.clickhouse.username`: ClickHouse HTTP username.
- `storage.clickhouse.password_env`: environment variable holding the ClickHouse password.
- `storage.clickhouse.batch_size`: number of rows buffered before each HTTP insert.
- `tui.enabled`: enable or disable the terminal UI.
- `tui.refresh_ms`: terminal UI refresh interval.
- `quote_rates`: optional direct quote conversion rates used by the TUI for cross-quote spread display.
- `venues`: adapter settings plus default quote/settle/margin assets.
- `venues.instruments`: the explicit instrument catalog for that venue instance.

Example ClickHouse storage config for the Zeabur service:

```toml
[storage]
mode = "clickhouse"
jsonl_dir = "data/bbo"

[storage.clickhouse]
url = "https://obdata.zeabur.app/"
database = "zeabur"
table = "bbo_ticks"
catalog_table = "instrument_catalog"
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
[[quote_rates]]
from = "USDC"
to = "USD"
rate = "1"

[[venues]]
venue_instance_id = "lighter"
adapter = "lighter"
enabled = true
url = "wss://mainnet.zklighter.elliot.ai/stream"
channel = "ticker"
default_quote_asset = "USDC"
default_settle_asset = "USDC"
default_margin_asset = "USDC"

[[venues.instruments]]
instrument_id = "0"
raw_symbol = "ETH"
feed_symbol = "0"
product_type = "perp"
base_asset = "ETH"
status = "active"

[[venues.instruments]]
instrument_id = "1"
raw_symbol = "BTC"
feed_symbol = "1"
product_type = "perp"
base_asset = "BTC"
status = "active"
```

`venue_instance_id` is the pricing domain, not just the adapter name. If a protocol exposes independent domains such as HIP3 markets, configure them as separate venue instances. `instrument_id` is the exchange/internal market ID; `feed_symbol` is the subscription key when it differs. Quote, settle, and margin assets default from the venue and can be overridden per instrument.

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
- `Left` / `Right`: switch selected base asset.
- `Up` / `Down`: switch selected instrument row.
- `1` / `2`: choose the first or second instrument leg in the spread panel.

## Output

When `storage.mode = "jsonl"` or `storage.mode = "both"`, ticks are written under:

```text
data/bbo/catalog/YYYY-MM-DD/<venue_instance_id>.jsonl
data/bbo/bbo/YYYY-MM-DD/<venue_instance_id>.jsonl
```

Catalog lines contain static instrument metadata such as base/quote assets and trading rules. BBO lines are narrow `BboTick` JSON objects. Common tick fields include:

- `instrument`: `catalog_id`, `venue_instance_id`, and `instrument_id`.
- `recv_ts_ns`: local receive timestamp in nanoseconds.
- `exchange_ts_ms`: exchange timestamp when provided by the feed.
- `sequence`: feed sequence/nonce when provided.
- `bid` / `ask`: best price, size, and optional order count.
- `spread`: `ask.price - bid.price`.
- `mid`: midpoint between bid and ask.
- `source`: source feed type such as `bbo`, `ticker`, or `l2_book`.
- `quality`: quality flags, including `inconsistent` for negative spread, `stale` for delayed exchange timestamps, and `gap` for refreshed orderbook gaps.

When `storage.mode = "clickhouse"` or `storage.mode = "both"`, catalog rows are inserted into `storage.clickhouse.catalog_table` and ticks into `storage.clickhouse.table`. The tick table includes:

- identifiers and timestamps: `catalog_id`, `venue_instance_id`, `instrument_id`, `recv_ts_ns`, `recv_time`, `exchange_ts_ms`, `sequence`, `source`.
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
