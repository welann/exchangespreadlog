#!/usr/bin/env python3
"""Generate an exchangespreadlog config from Lighter's highest-volume perp markets.

The ranking source is Lighter. Other venues are only used to resolve the
exchange-specific instrument identifier for the same base asset.
"""

from __future__ import annotations

import argparse
import json
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
from dataclasses import dataclass
from decimal import Decimal
from pathlib import Path
from typing import Optional


LIGHTER_ORDERBOOKS_URL = "https://mainnet.zklighter.elliot.ai/api/v1/orderBooks"
LIGHTER_DETAILS_URL = "https://mainnet.zklighter.elliot.ai/api/v1/orderBookDetails"
HYPERLIQUID_INFO_URL = "https://api.hyperliquid.xyz/info"
RISEX_MARKETS_URL = "https://api.rise.trade/v1/markets"
ZERO_ONE_INFO_URL = "https://zo-mainnet.n1.xyz/info"
ETHEREAL_PRODUCTS_URL = "https://api.ethereal.trade/v1/product"
PERPL_CONTEXT_URL = "https://app.perpl.xyz/api/v1/pub/context"

USER_AGENT = "exchangespreadlog-config-generator/0.1"
DETAIL_DELAY_SECONDS = 0.2


@dataclass(frozen=True)
class Instrument:
    instrument_id: str
    raw_symbol: str
    feed_symbol: str
    base_asset: str
    status: str = "active"
    price_tick: Optional[str] = None
    size_tick: Optional[str] = None


@dataclass(frozen=True)
class LighterMarket:
    market_id: str
    symbol: str
    base_asset: str
    volume: Decimal


@dataclass(frozen=True)
class Venue:
    venue_instance_id: str
    adapter: str
    url: str
    channel: str
    catalog_source: str
    default_quote_asset: str
    default_settle_asset: str
    default_margin_asset: str
    instruments: list[Instrument]
    metadata_url: Optional[str] = None


def get_json(url: str) -> object:
    request = urllib.request.Request(
        url,
        headers={"Accept": "application/json", "User-Agent": USER_AGENT},
    )
    with urllib.request.urlopen(request, timeout=20) as response:
        return json.load(response)


def post_json(url: str, payload: object) -> object:
    body = json.dumps(payload).encode("utf-8")
    request = urllib.request.Request(
        url,
        data=body,
        method="POST",
        headers={
            "Accept": "application/json",
            "Content-Type": "application/json",
            "User-Agent": USER_AGENT,
        },
    )
    with urllib.request.urlopen(request, timeout=20) as response:
        return json.load(response)


def as_decimal(value: object) -> Decimal:
    if value is None or value == "":
        return Decimal("0")
    return Decimal(str(value))


def normalize_base(symbol: object) -> str:
    base = str(symbol).upper().strip()
    base = base.split("[", 1)[0].strip()
    if ":" in base:
        base = base.split(":", 1)[1]
    if "/" in base:
        base = base.split("/", 1)[0]
    if "-" in base:
        base = base.split("-", 1)[0]
    for suffix in ("USDC", "USD"):
        if base.endswith(suffix) and len(base) > len(suffix):
            base = base[: -len(suffix)]
            break
    return base


def instrument(
    instrument_id: object,
    raw_symbol: object,
    feed_symbol: object,
    base_asset: object,
    status: str = "active",
    price_tick: Optional[str] = None,
    size_tick: Optional[str] = None,
) -> Instrument:
    return Instrument(
        instrument_id=str(instrument_id),
        raw_symbol=str(raw_symbol),
        feed_symbol=str(feed_symbol),
        base_asset=normalize_base(base_asset),
        status=status,
        price_tick=price_tick,
        size_tick=size_tick,
    )


def fetch_lighter_orderbooks() -> list[dict[str, object]]:
    data = get_json(LIGHTER_ORDERBOOKS_URL)
    if not isinstance(data, dict):
        raise RuntimeError("Lighter orderBooks returned a non-object payload")
    order_books = data.get("order_books")
    if not isinstance(order_books, list):
        raise RuntimeError("Lighter orderBooks payload is missing order_books")
    return [item for item in order_books if isinstance(item, dict)]


def parse_lighter_details_payload(data: object, source: str) -> list[dict[str, object]]:
    if not isinstance(data, dict):
        raise RuntimeError(f"Lighter {source} returned a non-object payload")
    details = data.get("order_book_details")
    if not isinstance(details, list):
        raise RuntimeError(f"Lighter {source} payload is missing order_book_details")
    return [item for item in details if isinstance(item, dict)]


def fetch_all_lighter_details() -> list[dict[str, object]]:
    data = get_json(LIGHTER_DETAILS_URL)
    return parse_lighter_details_payload(data, "orderBookDetails")


def fetch_lighter_detail(market_id: object) -> dict[str, object]:
    query = urllib.parse.urlencode({"market_id": str(market_id)})
    last_error: Optional[urllib.error.HTTPError] = None
    for attempt in range(3):
        try:
            data = get_json(f"{LIGHTER_DETAILS_URL}?{query}")
            details = parse_lighter_details_payload(data, f"orderBookDetails {market_id}")
            if not details:
                raise RuntimeError(f"Lighter detail {market_id} returned no details")
            return details[0]
        except urllib.error.HTTPError as err:
            last_error = err
            if err.code != 429 or attempt == 2:
                raise
            retry_after = err.headers.get("Retry-After")
            sleep_seconds = float(retry_after) if retry_after else 1.0 + attempt
            time.sleep(sleep_seconds)
    raise last_error or RuntimeError(f"Lighter detail {market_id} failed")


def fetch_lighter_details() -> list[dict[str, object]]:
    try:
        return fetch_all_lighter_details()
    except urllib.error.HTTPError as err:
        if err.code != 405:
            raise
        print(
            "Lighter full orderBookDetails returned 405; falling back to per-market details",
            file=sys.stderr,
        )

    details: list[dict[str, object]] = []
    for market in fetch_lighter_orderbooks():
        if str(market.get("market_type", "")).lower() != "perp":
            continue
        if str(market.get("status", "")).lower() != "active":
            continue
        if "market_id" not in market:
            continue
        details.append(fetch_lighter_detail(market["market_id"]))
        time.sleep(DETAIL_DELAY_SECONDS)
    return details


def fetch_top_lighter_markets(limit: int) -> list[LighterMarket]:
    ranked = [
        LighterMarket(
            market_id=str(market["market_id"]),
            symbol=str(market.get("symbol")),
            base_asset=normalize_base(market.get("symbol")),
            volume=as_decimal(market.get("daily_quote_token_volume")),
        )
        for market in fetch_lighter_details()
        if str(market.get("market_type", "")).lower() == "perp"
        and str(market.get("status", "")).lower() == "active"
        and "market_id" in market
        and market.get("symbol")
    ]

    top: list[LighterMarket] = []
    seen: set[str] = set()
    for market in sorted(ranked, key=lambda item: item.volume, reverse=True):
        if market.base_asset in seen:
            continue
        top.append(market)
        seen.add(market.base_asset)
        if len(top) == limit:
            break
    return top


def select_in_top_order(
    by_base: dict[str, Instrument],
    top_markets: list[LighterMarket],
) -> list[Instrument]:
    return [
        by_base[market.base_asset]
        for market in top_markets
        if market.base_asset in by_base
    ]


def fetch_hyperliquid_instruments(top_markets: list[LighterMarket]) -> list[Instrument]:
    data = post_json(HYPERLIQUID_INFO_URL, {"type": "allPerpMetas"})
    if isinstance(data, dict):
        metas = [data]
    elif isinstance(data, list):
        metas = [meta for meta in data if isinstance(meta, dict)]
    else:
        raise RuntimeError("Hyperliquid allPerpMetas returned an unsupported payload")

    by_base: dict[str, Instrument] = {}
    for meta in metas:
        universe = meta.get("universe")
        if not isinstance(universe, list):
            continue
        for market in universe:
            if not isinstance(market, dict) or market.get("isDelisted"):
                continue
            name = market.get("name")
            if not name:
                continue
            base = normalize_base(name)
            by_base.setdefault(base, instrument(name, name, name, base))
    return select_in_top_order(by_base, top_markets)


def fetch_risex_instruments(top_markets: list[LighterMarket]) -> list[Instrument]:
    data = get_json(RISEX_MARKETS_URL)
    if not isinstance(data, dict):
        raise RuntimeError("RiseX markets returned a non-object payload")
    markets = data.get("data", {}).get("markets") if isinstance(data.get("data"), dict) else None
    if not isinstance(markets, list):
        raise RuntimeError("RiseX markets payload is missing data.markets")

    best: dict[str, tuple[tuple[int, Decimal], Instrument]] = {}
    for market in markets:
        if not isinstance(market, dict):
            continue
        config = market.get("config") if isinstance(market.get("config"), dict) else {}
        raw_symbol = config.get("name") or market.get("display_name") or market.get("symbol")
        market_id = market.get("market_id")
        if not raw_symbol or market_id is None:
            continue
        active = bool(market.get("available")) and bool(market.get("visible", True)) and bool(
            market.get("active", True)
        )
        if not active:
            continue
        base = normalize_base(raw_symbol)
        candidate = instrument(market_id, raw_symbol, market_id, base)
        score = (
            0 if market.get("post_only") else 1,
            as_decimal(market.get("quote_volume_24h")),
        )
        if base not in best or score > best[base][0]:
            best[base] = (score, candidate)

    return select_in_top_order({base: item for base, (_, item) in best.items()}, top_markets)


def fetch_zero_one_instruments(top_markets: list[LighterMarket]) -> list[Instrument]:
    data = get_json(ZERO_ONE_INFO_URL)
    if not isinstance(data, dict) or not isinstance(data.get("markets"), list):
        raise RuntimeError("01 info payload is missing markets")

    by_base: dict[str, Instrument] = {}
    for market in data["markets"]:
        if not isinstance(market, dict):
            continue
        symbol = market.get("symbol")
        market_id = market.get("marketId")
        if symbol is None or market_id is None:
            continue
        base = normalize_base(symbol)
        by_base.setdefault(base, instrument(market_id, symbol, symbol, base))
    return select_in_top_order(by_base, top_markets)


def fetch_ethereal_instruments(top_markets: list[LighterMarket]) -> list[Instrument]:
    data = get_json(ETHEREAL_PRODUCTS_URL)
    products = None
    if isinstance(data, dict):
        products = data.get("data") or data.get("products")
    else:
        products = data
    if not isinstance(products, list):
        raise RuntimeError("Ethereal product payload is missing products")

    by_base: dict[str, Instrument] = {}
    for product in products:
        if not isinstance(product, dict):
            continue
        status = str(product.get("status", "")).upper()
        if status and status != "ACTIVE":
            continue
        ticker = product.get("ticker")
        if not ticker:
            continue
        raw_symbol = product.get("displayTicker") or ticker
        base = normalize_base(product.get("baseTokenName") or ticker)
        by_base.setdefault(base, instrument(ticker, raw_symbol, ticker, base))
    return select_in_top_order(by_base, top_markets)


def fetch_perpl_instruments(top_markets: list[LighterMarket]) -> list[Instrument]:
    data = get_json(PERPL_CONTEXT_URL)
    if not isinstance(data, dict) or not isinstance(data.get("markets"), list):
        raise RuntimeError("Perpl context payload is missing markets")

    by_base: dict[str, Instrument] = {}
    for market in data["markets"]:
        if not isinstance(market, dict):
            continue
        config = market.get("config") if isinstance(market.get("config"), dict) else {}
        if not config.get("is_open", False):
            continue
        market_id = market.get("id")
        raw_symbol = market.get("size_units") or market.get("symbol") or market.get("name")
        base = market.get("symbol") or market.get("name") or raw_symbol
        if market_id is None or not raw_symbol or not base:
            continue
        by_base.setdefault(
            normalize_base(base),
            instrument(
                market_id,
                raw_symbol,
                market_id,
                base,
                price_tick=decimal_tick(config.get("price_decimals")),
                size_tick=decimal_tick(config.get("size_decimals")),
            ),
        )
    return select_in_top_order(by_base, top_markets)


def lighter_instruments(top_markets: list[LighterMarket]) -> list[Instrument]:
    return [
        instrument(market.market_id, market.symbol, market.market_id, market.base_asset)
        for market in top_markets
    ]


def build_venues(top_markets: list[LighterMarket]) -> list[Venue]:
    return [
        Venue(
            venue_instance_id="hyperliquid",
            adapter="hyperliquid",
            url="wss://api.hyperliquid.xyz/ws",
            channel="bbo",
            catalog_source="exchange",
            metadata_url=HYPERLIQUID_INFO_URL,
            default_quote_asset="USDC",
            default_settle_asset="USDC",
            default_margin_asset="USDC",
            instruments=fetch_hyperliquid_instruments(top_markets),
        ),
        Venue(
            venue_instance_id="lighter",
            adapter="lighter",
            url="wss://mainnet.zklighter.elliot.ai/stream?readonly=true",
            channel="ticker",
            catalog_source="exchange",
            metadata_url=LIGHTER_ORDERBOOKS_URL,
            default_quote_asset="USDC",
            default_settle_asset="USDC",
            default_margin_asset="USDC",
            instruments=lighter_instruments(top_markets),
        ),
        Venue(
            venue_instance_id="risex",
            adapter="risex",
            url="wss://ws.rise.trade/ws",
            channel="orderbook",
            catalog_source="config",
            default_quote_asset="USDC",
            default_settle_asset="USDC",
            default_margin_asset="USDC",
            instruments=fetch_risex_instruments(top_markets),
        ),
        Venue(
            venue_instance_id="01",
            adapter="01",
            url="wss://zo-mainnet.n1.xyz",
            channel="deltas",
            catalog_source="config",
            default_quote_asset="USD",
            default_settle_asset="USD",
            default_margin_asset="USD",
            instruments=fetch_zero_one_instruments(top_markets),
        ),
        Venue(
            venue_instance_id="ethereal",
            adapter="ethereal",
            url="wss://ws2.ethereal.trade/v1/stream",
            channel="L2Book",
            catalog_source="exchange",
            metadata_url=ETHEREAL_PRODUCTS_URL,
            default_quote_asset="USD",
            default_settle_asset="USD",
            default_margin_asset="USD",
            instruments=fetch_ethereal_instruments(top_markets),
        ),
        Venue(
            venue_instance_id="perpl",
            adapter="perpl",
            url="wss://app.perpl.xyz/ws/v1/market-data",
            channel="order-book",
            catalog_source="exchange",
            metadata_url=PERPL_CONTEXT_URL,
            default_quote_asset="AUSD",
            default_settle_asset="AUSD",
            default_margin_asset="AUSD",
            instruments=fetch_perpl_instruments(top_markets),
        ),
    ]


def decimal_tick(decimals: object) -> Optional[str]:
    if decimals is None:
        return None
    decimals = int(decimals)
    if decimals <= 0:
        return "1"
    return "0." + ("0" * (decimals - 1)) + "1"


def toml_string(value: object) -> str:
    return json.dumps(str(value))


def toml_key_value(key: str, value: object) -> str:
    if isinstance(value, bool):
        return f"{key} = {'true' if value else 'false'}"
    return f"{key} = {toml_string(value)}"


def render_config(top_markets: list[LighterMarket], venues: list[Venue]) -> str:
    lines = [
        "# Generated by scripts/generate_config_from_lighter.py",
        "# Lighter ranking key: daily_quote_token_volume.",
        'mode = "bbo"',
        "",
        "[pipeline]",
        "channel_capacity = 4096",
        "stale_after_ms = 5000",
        "",
        "[storage]",
        'mode = "jsonl"',
        'jsonl_dir = "data/bbo"',
        "",
        "[storage.clickhouse]",
        'url = "https://obdata.zeabur.app/"',
        'database = "zeabur"',
        'table = "bbo_ticks"',
        'catalog_table = "instrument_catalog"',
        'username = "zeabur"',
        'password_env = "CLICKHOUSE_PASSWORD"',
        "create_table = true",
        "batch_size = 100",
        "",
        "[tui]",
        "enabled = true",
        "refresh_ms = 250",
        "",
        "[[quote_rates]]",
        'from = "USDC"',
        'to = "USD"',
        'rate = "1"',
        "",
        "[[quote_rates]]",
        'from = "USDT"',
        'to = "USD"',
        'rate = "1"',
        "",
        "[[quote_rates]]",
        'from = "AUSD"',
        'to = "USD"',
        'rate = "1"',
        "",
    ]

    for venue in venues:
        if not venue.instruments:
            continue
        lines.extend(
            [
                "[[venues]]",
                toml_key_value("venue_instance_id", venue.venue_instance_id),
                toml_key_value("adapter", venue.adapter),
                "enabled = true",
                toml_key_value("url", venue.url),
                toml_key_value("channel", venue.channel),
                toml_key_value("catalog_source", venue.catalog_source),
            ]
        )
        if venue.metadata_url:
            lines.append(toml_key_value("metadata_url", venue.metadata_url))
        lines.extend(
            [
                toml_key_value("default_quote_asset", venue.default_quote_asset),
                toml_key_value("default_settle_asset", venue.default_settle_asset),
                toml_key_value("default_margin_asset", venue.default_margin_asset),
                "",
            ]
        )
        for inst in venue.instruments:
            lines.extend(
                [
                    "[[venues.instruments]]",
                    toml_key_value("instrument_id", inst.instrument_id),
                    toml_key_value("raw_symbol", inst.raw_symbol),
                    toml_key_value("feed_symbol", inst.feed_symbol),
                    'product_type = "perp"',
                    toml_key_value("base_asset", inst.base_asset),
                    *(
                        [toml_key_value("price_tick", inst.price_tick)]
                        if inst.price_tick
                        else []
                    ),
                    *(
                        [toml_key_value("size_tick", inst.size_tick)]
                        if inst.size_tick
                        else []
                    ),
                    toml_key_value("status", inst.status),
                    "",
                ]
            )

    selected = ", ".join(
        f"{market.base_asset}:{market.market_id}:{market.volume}" for market in top_markets
    )
    lines.extend(
        [
            "# Selected Lighter markets:",
            f"# {selected}",
            "",
        ]
    )
    return "\n".join(lines)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Generate a config from Lighter top-volume perp markets."
    )
    parser.add_argument(
        "--output",
        default="config.generated.toml",
        help="Path to write the generated TOML config.",
    )
    parser.add_argument(
        "--limit",
        type=int,
        default=20,
        help="Number of Lighter base assets to select by 24h quote volume.",
    )
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    if args.limit <= 0:
        raise SystemExit("--limit must be positive")

    top_markets = fetch_top_lighter_markets(args.limit)
    venues = build_venues(top_markets)
    output = Path(args.output)
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(render_config(top_markets, venues), encoding="utf-8")

    print(
        "selected Lighter markets: "
        + ", ".join(f"{market.base_asset}({market.market_id})" for market in top_markets),
        file=sys.stderr,
    )
    for venue in venues:
        print(
            f"{venue.venue_instance_id}: {len(venue.instruments)} instruments",
            file=sys.stderr,
        )
    print(f"wrote {output}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
